//! What `comb get -v` adds on stderr: each key's read metadata, a tree with
//! the provider's shape whose every node keeps its own metadata under `_`
//! (as `<p>:cache._.decay` does), printed by the `-f` formatter as any object
//! is, and under `text` as `comb status --tree` draws its rows.
//!
//! A node's `_` holds its `age_ms`, whether a run is `executing`, its
//! `chain` and its `log`. The chain is every layer that could supply the
//! leaf, the one that answered first (`answered`), then each it hides, in
//! the order each supersedes the next: its `layer`, its `path` (as `comb
//! status` writes one) and, from `-vv`, the `value` it holds, with the lines
//! that layer raised under it. A line no one row raised is the node's: a
//! Source's that several leaves share sits on the node above them all.
//!
//! The count grows what is shown, as it lowers the log level: `-v` the
//! answering row and `error`, `fatal` and `warn` lines; `-vv` every row,
//! with values, and `info`; `-vvv` the rest of the leaf's `:cache` (`hash`,
//! `decay`, `poll`, `watch`) and `debug`. Everything but the lines is read
//! with a `:cache` read and a `layers` op made only under `-v`, so a read
//! without it asks the daemon nothing more.

use crate::cache::{CacheRow, LayerRecord, RowKind};
use crate::cli::status_format::{ChildMark, is_the_row, row_base, tree_display_rows};
use crate::cli::style::Glyphs;
use libbeachcomber::Session;
use libbeachcomber::path::Home;
use libbeachcomber::protocol::{Level, LogLine};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// The least log level `-v` given `count` times shows; `None` without it.
pub fn level(count: u8) -> Option<Level> {
    match count {
        0 => None,
        1 => Some(Level::Warn),
        2 => Some(Level::Info),
        _ => Some(Level::Debug),
    }
}

/// One node of a read's metadata tree.
#[derive(Debug, Default, PartialEq)]
pub struct Node {
    /// What the node's `:cache` answer says of it: `age_ms`, `executing`,
    /// and at `-vvv` the rest.
    own: Map<String, Value>,
    chain: Vec<Row>,
    /// The lines that are the node's own, not one row's.
    log: Vec<LogLine>,
    children: BTreeMap<String, Node>,
}

/// One layer of a leaf's chain.
#[derive(Debug, PartialEq)]
struct Row {
    /// The `layers` record, as `comb status --tree` draws it, and where it
    /// stands in the read's order.
    record: LayerRecord,
    order: usize,
    /// `put`, `source(<name>)`, `config(<file>)`, `computed(<line>)`,
    /// `template` or `memo`.
    layer: String,
    /// The directory it is stored at, as stored (`None`: the global entry).
    at: Option<String>,
    /// The same, as `comb status` writes it.
    path: String,
    /// What it holds; `None` below `-vv`.
    value: Option<Value>,
    answered: bool,
    log: Vec<LogLine>,
}

/// The metadata of one key's read: what the read itself knew (`age_ms`, the
/// oldest input it used, and its log lines), with what the daemon says of the
/// records behind it.
pub fn metadata(
    session: &mut Session,
    key: &str,
    path: Option<&str>,
    age_ms: Option<u128>,
    log: Vec<LogLine>,
    count: u8,
) -> Node {
    let home = Home::from_env();
    let (cache, layers) = daemon_keys(key).map_or((None, None), |(cache_key, layers_key)| {
        (
            session.get(&cache_key, path).ok().flatten(),
            session.layers(&layers_key, path).ok().flatten(),
        )
    });
    let mut root = build(cache.as_ref(), layers.as_ref(), count, &home);
    if !root.own.contains_key("age_ms")
        && let Some(age) = age_ms
    {
        root.own.insert("age_ms".into(), (age as u64).into());
    }
    attach(&mut root, key, log, &home);
    root
}

/// The `:cache` key and the `layers` key of `key`, or `None` for a key the
/// daemon keeps no records for: `env.*`, and a facet's.
fn daemon_keys(key: &str) -> Option<(String, String)> {
    let (head, address) = libbeachcomber::key::split_key(key).ok()?;
    let (provider, facet) = libbeachcomber::key::split_facet(head).ok()?;
    if facet.is_some() || provider == "env" {
        return None;
    }
    Some((
        libbeachcomber::key::join(&format!("{provider}:cache"), &address),
        libbeachcomber::key::join(provider, &address),
    ))
}

/// A `:cache` leaf answer: an object whose `source` is text. A node's child
/// called `source` is an object.
fn is_leaf(v: &Value) -> bool {
    v.get("source").is_some_and(Value::is_string)
}

/// The metadata tree of a `layers` answer, walked beside the `:cache` one:
/// an array is a leaf's chain, an object a node.
fn build(cache: Option<&Value>, layers: Option<&Value>, count: u8, home: &Home) -> Node {
    let mut node = Node::default();
    if let Some(cache) = cache.filter(|c| is_leaf(c)) {
        node.own = own(cache, count);
    }
    match layers {
        Some(Value::Array(stack)) => node.chain = chain(stack, count, home),
        Some(Value::Object(map)) => {
            for (k, sub) in map {
                let child = build(cache.and_then(|c| c.get(k)), Some(sub), count, home);
                node.children.insert(k.clone(), child);
            }
        }
        _ => {}
    }
    node
}

/// A leaf's own fields from its `:cache` answer: `age_ms` and `executing`,
/// and at `-vvv` the rest but `source`, which its chain says.
fn own(cache: &Value, count: u8) -> Map<String, Value> {
    let Some(all) = cache.as_object() else {
        return Map::new();
    };
    all.iter()
        .filter(|(k, _)| match k.as_str() {
            "age_ms" | "executing" => true,
            "source" => false,
            _ => count >= 3,
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// A leaf's chain from its `layers` stack: the answering layer first, then
/// the rest as the read stacks them; below `-vv` the answering one alone,
/// without its value.
fn chain(stack: &[Value], count: u8, home: &Home) -> Vec<Row> {
    let row = |(order, l): (usize, &Value)| {
        let label = l.get("label").and_then(Value::as_str).unwrap_or("?");
        // A template's or memo's record is labelled by where it came from;
        // its facet says what it is.
        let layer = l.get("facet").and_then(Value::as_str).unwrap_or(label);
        let at = l.get("path").and_then(Value::as_str).map(str::to_string);
        Row {
            record: serde_json::from_value(l.clone()).unwrap_or_else(|_| LayerRecord {
                label: label.to_string(),
                facet: None,
                path: at.clone(),
                value: Value::Null,
                age_ms: 0,
                answered: false,
            }),
            order,
            layer: layer.to_string(),
            path: at
                .as_deref()
                .map_or_else(|| "~".to_string(), |p| home.compact(p)),
            at,
            value: (count >= 2).then(|| l.get("value").cloned().unwrap_or(Value::Null)),
            answered: l.get("answered") == Some(&Value::Bool(true)),
            log: Vec::new(),
        }
    };
    let (answered, hidden): (Vec<_>, Vec<_>) = stack
        .iter()
        .enumerate()
        .partition(|(_, l)| l.get("answered") == Some(&Value::Bool(true)));
    let rows = answered.into_iter().map(row);
    if count >= 2 {
        rows.chain(hidden.into_iter().map(row)).collect()
    } else {
        rows.collect()
    }
}

/// Put each line where it belongs: a Source's under its row in the one leaf
/// whose chain shows that row, or, when several do, on the node above them
/// all; a render's or memo's (it names its leaf) under that leaf's answering
/// template or memo row, else on the leaf; anything else on the root.
fn attach(root: &mut Node, key: &str, log: Vec<LogLine>, home: &Home) {
    let base = address_of(key);
    for mut line in log {
        if line.origin.starts_with("source(") {
            let mut at = Vec::new();
            showing(root, &mut Vec::new(), &line, &mut at);
            if let [only] = at.as_slice()
                && let Some(row) =
                    node_at(root, only).and_then(|n| n.chain.iter_mut().find(|r| raised(r, &line)))
            {
                line.path = None;
                row.log.push(line);
                continue;
            }
            let common = at.split_first().map_or_else(Vec::new, |(first, rest)| {
                let mut common = first.clone();
                for other in rest {
                    let same = common.iter().zip(other).take_while(|(a, b)| a == b).count();
                    common.truncate(same);
                }
                common
            });
            line.path = Some(
                line.path
                    .as_deref()
                    .map_or_else(|| "~".into(), |p| home.compact(p)),
            );
            node_at(root, &common)
                .expect("a prefix of a leaf's address is a node")
                .log
                .push(line);
            continue;
        }
        let below = line
            .key
            .take()
            .and_then(|k| relative(&base, &address_of(&k)));
        let Some(node) = below.and_then(|segments| node_at(root, &segments)) else {
            root.log.push(line);
            continue;
        };
        match node
            .chain
            .iter_mut()
            .find(|r| r.answered && matches!(r.layer.as_str(), "template" | "memo"))
        {
            Some(row) => row.log.push(line),
            None => node.log.push(line),
        }
    }
}

/// Whether `row` is the layer that raised `line`.
fn raised(row: &Row, line: &LogLine) -> bool {
    row.layer == line.origin && row.at == line.path
}

/// The addresses (as segments) of the leaves under `node` whose chain shows
/// the row that raised `line`.
fn showing(node: &Node, at: &mut Vec<String>, line: &LogLine, out: &mut Vec<Vec<String>>) {
    if node.chain.iter().any(|r| raised(r, line)) {
        out.push(at.clone());
    }
    for (k, child) in &node.children {
        at.push(k.clone());
        showing(child, at, line, out);
        at.pop();
    }
}

fn node_at<'n>(node: &'n mut Node, segments: &[String]) -> Option<&'n mut Node> {
    match segments.split_first() {
        None => Some(node),
        Some((first, rest)) => node_at(node.children.get_mut(first)?, rest),
    }
}

/// A key's head and its address's segments.
fn address_of(key: &str) -> (String, Vec<String>) {
    match libbeachcomber::key::split_key(key) {
        Ok((head, address)) => (
            head.to_string(),
            libbeachcomber::key::segments(&address)
                .into_iter()
                .map(|s| s.into_owned())
                .collect(),
        ),
        Err(_) => (key.to_string(), Vec::new()),
    }
}

/// Where `key` lies below `base`, as segments; `None` when it does not.
fn relative(base: &(String, Vec<String>), key: &(String, Vec<String>)) -> Option<Vec<String>> {
    (base.0 == key.0 && key.1.starts_with(&base.1)).then(|| key.1[base.1.len()..].to_vec())
}

impl Node {
    /// The tree as `-f` prints it: each node's own metadata under `_`.
    pub fn to_json(&self) -> Value {
        let mut own = self.own.clone();
        if !self.chain.is_empty() {
            let rows: Vec<Value> = self.chain.iter().map(Row::to_json).collect();
            own.insert("chain".into(), rows.into());
        }
        if !self.log.is_empty() {
            own.insert("log".into(), lines_json(&self.log));
        }
        let mut map = Map::new();
        if !own.is_empty() {
            map.insert("_".into(), own.into());
        }
        for (k, child) in &self.children {
            map.insert(k.clone(), child.to_json());
        }
        map.into()
    }
}

impl Row {
    fn to_json(&self) -> Value {
        let mut row = Map::new();
        row.insert("layer".into(), self.layer.clone().into());
        row.insert("path".into(), self.path.clone().into());
        if let Some(value) = &self.value {
            row.insert("value".into(), value.clone());
        }
        if self.answered {
            row.insert("answered".into(), true.into());
        }
        if !self.log.is_empty() {
            row.insert("log".into(), lines_json(&self.log));
        }
        row.into()
    }
}

fn lines_json(lines: &[LogLine]) -> Value {
    serde_json::to_value(lines).expect("log lines serialise")
}

/// One key's read, as `text` draws it: the key, its metadata and the value
/// it came to.
pub struct Read<'a> {
    pub key: &'a str,
    pub node: &'a Node,
    pub value: Option<&'a Value>,
}

/// What every row of a `text` drawing shares.
pub struct Drawing<'a> {
    /// The directory read from, as stored (`None`: the global entry): a
    /// rendered row's PATH.
    pub dir: Option<&'a str>,
    pub count: u8,
    pub ascii: bool,
    pub color: bool,
    /// The total width the columns fit; `None` uncut.
    pub width: Option<usize>,
    /// The daemon's `status` rows (`-vvv`), whose POLL and TTL a Source's
    /// answer shows.
    pub status: &'a [CacheRow],
}

/// The metadata of every key read, as `text` prints it for a person: the
/// rows `comb status --tree` draws, under one header. Per leaf, the field
/// row is the answer, marked; from `-vv` the layers behind it hang under it,
/// as `--tree` hangs them; each line hangs under the row that raised it,
/// marked as a line, not a layer, and a line no single row raised hangs
/// under the field row, saying where it came from.
///
/// ```text
/// FIELD         SOURCE        VALUE   PATH    AGE
/// git.branch    put           ▸ demo  ~/proj  0.0s
/// └─ shadowed   source(head)  main    ~/proj    4s
///    ┆          [warn] index.lock present
/// ```
pub fn human(reads: &[Read<'_>], drawing: &Drawing<'_>) -> String {
    let mut sheet = Sheet::default();
    for read in reads {
        let mut named = Vec::new();
        group(
            read.key, read.node, read.value, drawing, &mut named, &mut sheet,
        );
    }
    crate::cli::status_format::draw(
        &sheet.rows,
        &sheet.marks,
        drawing.color,
        drawing.width,
        drawing.ascii,
        &crate::cli::status_format::read_shape(drawing.count >= 3),
    )
}

/// The rows drawn, and how each is marked ([`ChildMark`]; `None` a field row).
#[derive(Default)]
struct Sheet {
    rows: Vec<CacheRow>,
    marks: Vec<Option<ChildMark>>,
}

impl Sheet {
    fn push(&mut self, row: CacheRow, mark: Option<ChildMark>) {
        self.rows.push(row);
        self.marks.push(mark);
    }

    /// `log` as lines under the row above; `named`, each says where it came
    /// from, as no row above it does.
    fn lines(&mut self, log: &[LogLine], named: bool, under: Option<bool>, glyphs: &Glyphs) {
        for line in log {
            let mut text = line.msg.clone();
            if named {
                let from = match &line.path {
                    Some(path) => format!("{} {path}", line.origin),
                    None => line.origin.clone(),
                };
                text = format!("{from}: {text}");
            }
            if let Some(n) = line.count {
                text.push_str(&format!(" ({}{n})", glyphs.times));
            }
            self.push(
                CacheRow {
                    value: text.into(),
                    ..CacheRow::default()
                },
                Some(ChildMark {
                    last: false,
                    answered: true,
                    template: false,
                    line: Some(line.level),
                    under,
                }),
            );
        }
    }
}

/// The rows of `node`, read as `key`: a leaf's group, or each leaf's below
/// it. `named` holds the lines no single row raised that are still to be
/// drawn: the first field row below takes them.
fn group(
    key: &str,
    node: &Node,
    value: Option<&Value>,
    drawing: &Drawing<'_>,
    named: &mut Vec<LogLine>,
    sheet: &mut Sheet,
) {
    named.extend(node.log.iter().cloned());
    if !node.children.is_empty() {
        for (k, child) in &node.children {
            let below = &libbeachcomber::key::push(key, k);
            group(
                below,
                child,
                value.and_then(|v| v.get(k)),
                drawing,
                named,
                sheet,
            );
        }
        return;
    }
    let glyphs = Glyphs::of(drawing.ascii);
    let row = field_row(key, node, value, drawing);
    // The layers `--tree` hangs under the row, in the read's order, from -vv.
    let mut layers: Vec<&Row> = node.chain.iter().collect();
    layers.sort_by_key(|r| r.order);
    let records: Vec<LayerRecord> = if drawing.count >= 2 {
        layers.iter().map(|r| r.record.clone()).collect()
    } else {
        Vec::new()
    };
    let (display, marks) = tree_display_rows(std::slice::from_ref(&row), &[records]);
    let drawn: Vec<&Row> = layers
        .iter()
        .copied()
        .filter(|r| drawing.count >= 2 && !is_the_row(&row, &r.record))
        .collect();
    let mut display = display.into_iter().zip(marks);
    let (field, mark) = display.next().expect("the field row");
    sheet.push(field, mark);
    // A row not drawn is the field row's own: its lines are the answer's.
    for r in layers.iter().filter(|r| !drawn.contains(r)) {
        sheet.lines(&r.log, false, None, glyphs);
    }
    sheet.lines(named, true, None, glyphs);
    named.clear();
    for ((layer, mark), r) in display.zip(drawn) {
        let last = mark.is_some_and(|m| m.last);
        sheet.push(layer, mark);
        sheet.lines(&r.log, false, Some(last), glyphs);
    }
}

/// A leaf's field row, as `comb status` would show the answer: the record
/// that answered, or for a template's or memo's answer the row rendered here,
/// at the directory read from. At `-vvv` a Source's answer carries its
/// status row's lifecycle, for POLL and TTL.
fn field_row(key: &str, node: &Node, value: Option<&Value>, drawing: &Drawing<'_>) -> CacheRow {
    let (provider, address) = address_of(key);
    let provider = row_base(&provider).to_string();
    let age_ms = node
        .own
        .get("age_ms")
        .and_then(Value::as_u64)
        .map(u128::from);
    let rendered = |source: &str| CacheRow {
        provider: provider.clone(),
        path: drawing.dir.map(str::to_string),
        source: source.to_string(),
        field: key.to_string(),
        value: value.cloned().unwrap_or(Value::Null),
        age_ms,
        kind: Some(RowKind::Rendered),
        ..CacheRow::default()
    };
    let Some(record) = node.chain.iter().find(|r| r.answered).map(|r| &r.record) else {
        // Nothing the daemon holds answered: an `env.*` key, a facet's, a miss.
        let source = if value.is_some() { "rendered" } else { "-" };
        return CacheRow {
            kind: None,
            ..rendered(source)
        };
    };
    match record.facet.as_deref() {
        Some("memo") => rendered("memo"),
        Some(_) => rendered("rendered"),
        None => {
            let mut row = CacheRow {
                provider: provider.clone(),
                path: record.path.clone(),
                source: record.label.clone(),
                field: key.to_string(),
                value: record.value.clone(),
                age_ms,
                ..CacheRow::default()
            };
            let top = address.first().map(String::as_str);
            if let Some(status) = drawing.status.iter().find(|s| {
                s.provider == provider
                    && s.path == record.path
                    && s.source == record.label
                    && Some(s.field.as_str()) == top
            }) {
                row.kind = status.kind.clone();
                row.poll_interval_secs = status.poll_interval_secs;
                row.poll_count = status.poll_count;
                row.polls_elapsed = status.polls_elapsed;
                row.next_poll_in_secs = status.next_poll_in_secs;
                row.failure = status.failure.clone();
            }
            row
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn layers() -> Value {
        json!([
            {"label": "put", "path": "/w/proj", "value": "main", "age_ms": 9, "answered": true},
            {"label": "source(head)", "path": null, "value": "feature/x", "age_ms": 2100}
        ])
    }

    fn line(level: Level, origin: &str, path: Option<&str>, msg: &str) -> LogLine {
        let mut line = LogLine::new(level, origin, msg);
        line.path = path.map(str::to_string);
        line
    }

    #[test]
    fn each_count_shows_more_of_the_chain_and_of_cache() {
        let cache = json!({"age_ms": 2100, "source": "put", "executing": false,
            "hash": "9c6001d328a94690", "decay": 4});
        let home = Home::literal(Some("/w"));
        let one = build(Some(&cache), Some(&layers()), 1, &home).to_json();
        assert_eq!(
            one,
            json!({"_": {"age_ms": 2100, "executing": false,
                "chain": [{"layer": "put", "path": "~/proj", "answered": true}]}})
        );
        let two = build(Some(&cache), Some(&layers()), 2, &home).to_json();
        assert_eq!(
            two["_"]["chain"],
            json!([{"layer": "put", "path": "~/proj", "value": "main", "answered": true},
                   {"layer": "source(head)", "path": "~", "value": "feature/x"}])
        );
        assert!(two["_"].get("hash").is_none());
        let three = build(Some(&cache), Some(&layers()), 3, &home).to_json();
        assert_eq!(three["_"]["hash"], "9c6001d328a94690");
        assert!(three["_"].get("source").is_none());
    }

    /// `layers()` read as `git.branch` at `count`, with a line from the
    /// hidden Source and one from the client, as `text` draws it.
    fn drawn(count: u8) -> String {
        let home = Home::literal(Some("/w"));
        // Global entries, so the table's own home leaves the paths as `~`.
        let stack = json!([
            {"label": "put", "path": null, "value": "main", "age_ms": 9, "answered": true},
            {"label": "source(head)", "path": null, "value": "feature/x", "age_ms": 2100}
        ]);
        let mut node = build(None, Some(&stack), count, &home);
        node.own.insert("age_ms".into(), 2100.into());
        let log = vec![
            line(Level::Warn, "source(head)", None, "index.lock present"),
            line(Level::Info, "client", None, "noted"),
        ];
        attach(&mut node, "git.branch", log, &home);
        let read = Read {
            key: "git.branch",
            node: &node,
            value: Some(&json!("main")),
        };
        human(
            &[read],
            &Drawing {
                dir: None,
                count,
                ascii: true,
                color: false,
                width: None,
                status: &[],
            },
        )
    }

    #[test]
    fn text_is_status_trees_rows_with_each_line_under_its_row() {
        assert_eq!(
            drawn(2),
            "FIELD        SOURCE        VALUE      PATH   AGE\n\
             git.branch   put           > main     ~     2.1s\n\
             \x20 :          [info] client: noted\n\
             `- shadowed  source(head)  feature/x  ~       2s\n\
             \x20    :       [warn] index.lock present\n"
        );
        // -v: the field row alone; the hidden layer's line names it.
        assert_eq!(
            drawn(1),
            "FIELD       SOURCE  VALUE   PATH   AGE\n\
             git.branch  put     > main  ~     2.1s\n\
             \x20 :         [warn] source(head) ~: index.lock present\n\
             \x20 :         [info] client: noted\n"
        );
    }

    #[test]
    fn a_line_several_leaves_share_sits_on_the_node_above_them() {
        let home = Home::literal(Some("/w"));
        let stack = json!([{"label": "source(head)", "path": "/w/p", "value": 1, "age_ms": 0,
            "answered": true}]);
        let tree = json!({"a": stack, "g": {"b": stack}, "t": [{"label": "config(/c.toml)",
            "facet": "template", "path": null, "value": "{{ x }}", "age_ms": 0, "answered": true}]});
        let mut node = build(None, Some(&tree), 1, &home);
        let mut failed = LogLine::new(Level::Error, "template", "p.t: broken");
        failed.key = Some("p.t".into());
        attach(
            &mut node,
            "p",
            vec![
                line(Level::Warn, "source(head)", Some("/w/p"), "shared"),
                failed,
            ],
            &home,
        );
        let json = node.to_json();
        assert_eq!(json["_"]["log"][0]["msg"], "shared");
        assert_eq!(json["_"]["log"][0]["path"], "~/p");
        assert_eq!(json["t"]["_"]["chain"][0]["log"][0]["msg"], "p.t: broken");
        assert!(json["t"]["_"]["chain"][0]["log"][0].get("key").is_none());
    }

    #[test]
    fn a_facet_or_env_key_has_no_records() {
        assert_eq!(daemon_keys("env.HOME"), None);
        assert_eq!(daemon_keys("git:put.branch"), None);
        assert_eq!(
            daemon_keys("git.branch"),
            Some(("git:cache.branch".into(), "git.branch".into()))
        );
        assert_eq!(daemon_keys("git"), Some(("git:cache".into(), "git".into())));
    }
}
