//! The one deep merge of a value tree: `over` laid on `base`, recursing into
//! objects present on both sides; anything else on either side — a scalar, or
//! an array, which is a leaf — replaces what `base` holds.
//!
//! Generic over the object's member map, so the daemon's `provider::Value`
//! tree and JSON merge by the same rule. It never converts a value: numbers
//! and nesting depth are whatever the inputs hold.

use serde_json::{Map, Value as Json};

/// An object's members in a tree whose values are `V`.
pub trait Members<V>: IntoIterator<Item = (String, V)> + Sized {
    /// The member at `key`, if any.
    fn member_mut(&mut self, key: &str) -> Option<&mut V>;
    /// Set the member at `key`, replacing what was there.
    fn set_member(&mut self, key: String, value: V);
    /// `value`'s members, when it is an object.
    fn as_object(value: &mut V) -> Option<&mut Self>;
    /// `value`'s members, when it is an object; `value` itself otherwise.
    fn into_object(value: V) -> Result<Self, V>;
}

/// Merge `over` into `base`, `over` winning per leaf.
pub fn deep_merge<V, M: Members<V>>(base: &mut M, over: M) {
    for (key, value) in over {
        if let Some(existing) = base.member_mut(&key).and_then(M::as_object) {
            match M::into_object(value) {
                Ok(incoming) => deep_merge(existing, incoming),
                Err(value) => base.set_member(key, value),
            }
        } else {
            base.set_member(key, value);
        }
    }
}

impl Members<Json> for Map<String, Json> {
    fn member_mut(&mut self, key: &str) -> Option<&mut Json> {
        self.get_mut(key)
    }
    fn set_member(&mut self, key: String, value: Json) {
        self.insert(key, value);
    }
    fn as_object(value: &mut Json) -> Option<&mut Self> {
        value.as_object_mut()
    }
    fn into_object(value: Json) -> Result<Self, Json> {
        match value {
            Json::Object(map) => Ok(map),
            other => Err(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn merged(base: Json, over: Json) -> Json {
        let (Json::Object(mut b), Json::Object(o)) = (base, over) else {
            panic!("objects");
        };
        deep_merge(&mut b, o);
        Json::Object(b)
    }

    #[test]
    fn objects_merge_per_key_and_over_wins_a_leaf() {
        assert_eq!(
            merged(
                json!({"a": 1, "o": {"x": 1, "y": 1}}),
                json!({"a": 2, "o": {"y": 2}})
            ),
            json!({"a": 2, "o": {"x": 1, "y": 2}})
        );
    }

    #[test]
    fn anything_but_two_objects_replaces() {
        assert_eq!(
            merged(json!({"o": {"x": 1}}), json!({"o": 9})),
            json!({"o": 9})
        );
        assert_eq!(
            merged(json!({"o": 9}), json!({"o": {"x": 1}})),
            json!({"o": {"x": 1}})
        );
        // Arrays are leaves: replaced whole, not appended or merged by index.
        assert_eq!(
            merged(json!({"l": [1, 2, 3]}), json!({"l": [4]})),
            json!({"l": [4]})
        );
        // Null is a value to this merge, not a removal (that is `merge()`'s).
        assert_eq!(
            merged(json!({"a": 1}), json!({"a": null})),
            json!({"a": null})
        );
    }

    #[test]
    fn numbers_pass_through_unconverted() {
        assert_eq!(
            merged(json!({"i": 1, "f": 1.5}), json!({"w": 2.0})),
            json!({"i": 1, "f": 1.5, "w": 2.0})
        );
        let Json::Object(m) = merged(json!({}), json!({"w": 2.0})) else {
            unreachable!()
        };
        assert!(m["w"].is_f64(), "a whole float stays a float");
    }
}
