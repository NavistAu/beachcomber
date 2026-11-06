//! Lazy `comb`: the object a render reads `comb[...]` through.
//!
//! Ref discovery reads what a template names from its text. A ref used other
//! than by attribute — subscripted, filtered, iterated — names the node at the
//! end of its literal attribute chain, and that node is read whole:
//! `comb.aws_profiles[name]` reads `aws_profiles`, `git[field]` resolves `git`.
//! `comb` alone is the one name that cannot be read whole — it is every
//! provider — so `comb[...]` is bound to a [`LazyNode`] that reads a member
//! when the render asks for it: `comb['git:cache']` reads `git:cache`,
//! `comb[name]` the provider `name` names.
//!
//! A node asks through a [`Bridge`]. The resolver owns the fetch memo and an
//! engine object cannot borrow it, so a render with a lazy `comb` runs on a
//! scoped thread while the resolver's own thread answers what the node asks
//! ([`crate::resolve`]). Every answer is a fetch as an eager ref's is: once
//! per `(key, path)`, and recorded as a dependency.

use crate::resolve::ResolveError;
use minijinja::value::{Enumerator, Object, ObjectRepr, Value as MjValue};
use serde_json::Value as JsonValue;
use std::collections::HashSet;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};

/// The resolver's answer to a read: the data, `None` for nothing there.
pub(crate) type Answer = Result<Option<JsonValue>, ResolveError>;

/// The render thread's line to the resolver: a key to read, and where to
/// send the data.
///
/// An engine object cannot return an error, so the first one an answer
/// carries is kept here and the node reads as nothing; the render's result is
/// discarded for it afterwards ([`Bridge::take_error`]).
pub(crate) struct Bridge {
    asks: Mutex<mpsc::Sender<(String, mpsc::SyncSender<Answer>)>>,
    error: Mutex<Option<ResolveError>>,
}

impl Bridge {
    pub(crate) fn new(asks: mpsc::Sender<(String, mpsc::SyncSender<Answer>)>) -> Self {
        Self {
            asks: Mutex::new(asks),
            error: Mutex::new(None),
        }
    }

    /// Ask the resolver for the data at `key`, and wait for it.
    fn read(&self, key: String) -> Option<JsonValue> {
        let (reply, answer) = mpsc::sync_channel(1);
        let sent = self
            .asks
            .lock()
            .map(|asks| asks.send((key, reply)).is_ok())
            .unwrap_or(false);
        if !sent {
            return None;
        }
        match answer.recv() {
            Ok(Ok(value)) => value,
            Ok(Err(e)) => {
                if let Ok(mut slot) = self.error.lock() {
                    slot.get_or_insert(e);
                }
                None
            }
            Err(_) => None,
        }
    }

    /// The first error an answer carried, if any.
    pub(crate) fn take_error(&self) -> Option<ResolveError> {
        self.error.lock().ok().and_then(|mut slot| slot.take())
    }
}

/// What discovery read under `comb` before the render: the keys it read
/// exactly (`comb.git`, `comb.git.branch`), and every key it only passed
/// through on the way to one (`git`, for `comb.git.branch`).
#[derive(Default)]
pub(crate) struct Prefetch {
    exact: HashSet<String>,
    through: HashSet<String>,
}

impl Prefetch {
    /// What `refs` read under `comb`.
    pub(crate) fn of(refs: &[crate::virtual_fields::Ref]) -> Self {
        use crate::virtual_fields::Ref;
        let mut prefetch = Self::default();
        for r in refs {
            match r {
                Ref::CombProvider(provider) => {
                    prefetch.exact.insert(provider.clone());
                }
                Ref::CombField(provider, field) => {
                    prefetch.through.insert(provider.clone());
                    prefetch.exact.insert(crate::key::join(provider, field));
                }
                Ref::Env(_) | Ref::Resolved(..) | Ref::Dynamic(_) => {}
            }
        }
        prefetch
    }
}

/// The `comb` root, or a provider below it that discovery passed through.
///
/// `prefetched` is what discovery already read at and below this node: a
/// member read exactly is answered from it, a provider it passed through is
/// another node over the prefetch below it, and any other member is read when
/// asked for — exactly that member. A provider node read whole (iterated,
/// tested, printed, returned) is read once and kept.
pub(crate) struct LazyNode {
    bridge: Arc<Bridge>,
    /// `None` is the root, whose members are provider heads.
    provider: Option<String>,
    prefetched: Option<serde_json::Map<String, JsonValue>>,
    prefetch: Arc<Prefetch>,
    whole: OnceLock<Option<JsonValue>>,
}

impl std::fmt::Debug for LazyNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.provider {
            Some(provider) => write!(f, "<comb.{provider}>"),
            None => f.write_str("<comb>"),
        }
    }
}

impl LazyNode {
    /// The `comb` root over what discovery read under it.
    pub(crate) fn comb(
        bridge: Arc<Bridge>,
        prefetched: Option<serde_json::Map<String, JsonValue>>,
        prefetch: Arc<Prefetch>,
    ) -> Self {
        Self {
            bridge,
            provider: None,
            prefetched,
            prefetch,
            whole: OnceLock::new(),
        }
    }

    /// The key of the member `name`: a provider head at the root — a name
    /// that is not one (`comb['a.b']`) names nothing — and a segment below a
    /// provider.
    fn member_key(&self, name: &str) -> Option<String> {
        match &self.provider {
            None => matches!(crate::key::split_key(name), Ok((head, address))
                    if head == name && address.is_empty())
            .then(|| name.to_string()),
            Some(provider) => Some(crate::key::push(provider, name)),
        }
    }

    /// The provider node whole, read once. `None` for the root.
    fn whole(&self) -> Option<&JsonValue> {
        let provider = self.provider.as_ref()?;
        self.whole
            .get_or_init(|| self.bridge.read(provider.clone()))
            .as_ref()
    }
}

/// A member's name as a key segment: a string, or an integer index.
fn segment_of(key: &MjValue) -> Option<String> {
    if let Some(s) = key.as_str() {
        return Some(s.to_string());
    }
    i64::try_from(key.clone())
        .ok()
        .filter(|n| *n >= 0)
        .map(|n| n.to_string())
}

impl Object for LazyNode {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Map
    }

    fn get_value(self: &Arc<Self>, key: &MjValue) -> Option<MjValue> {
        let name = segment_of(key)?;
        let member = self.member_key(&name)?;
        if let Some(whole) = self.whole.get() {
            return whole
                .as_ref()
                .and_then(|w| w.get(&name))
                .map(MjValue::from_serialize);
        }
        let prefetched = self.prefetched.as_ref().and_then(|p| p.get(&name));
        if self.prefetch.exact.contains(&member) {
            return prefetched.map(MjValue::from_serialize);
        }
        if self.provider.is_none() && self.prefetch.through.contains(&member) {
            return Some(MjValue::from_object(Self {
                bridge: self.bridge.clone(),
                provider: Some(member),
                prefetched: match prefetched {
                    Some(JsonValue::Object(below)) => Some(below.clone()),
                    _ => None,
                },
                prefetch: self.prefetch.clone(),
                whole: OnceLock::new(),
            }));
        }
        self.bridge
            .read(member)
            .map(|v| MjValue::from_serialize(&v))
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        if self.provider.is_none() {
            return Enumerator::NonEnumerable;
        }
        match self.whole() {
            Some(JsonValue::Object(map)) => {
                Enumerator::Values(map.keys().map(|k| MjValue::from(k.as_str())).collect())
            }
            _ => Enumerator::Empty,
        }
    }

    fn is_true(self: &Arc<Self>) -> bool {
        self.provider.is_none()
            || self
                .whole()
                .is_some_and(|w| w != &JsonValue::Object(Default::default()))
    }
}

/// The value a lazy node stands for, when `value` is one: a provider node
/// whole (`None` when nothing is there), and nothing for the root.
pub(crate) fn as_whole(value: &MjValue) -> Option<Option<JsonValue>> {
    let node = value.downcast_object_ref::<LazyNode>()?;
    Some(node.whole().cloned())
}
