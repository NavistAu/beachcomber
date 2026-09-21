//! One JSON→`Value` conversion, shared by every ingestion path.
//!
//! Four independent copies of this conversion once existed — in `put`, script,
//! http and library providers — and all four stringified nested objects instead
//! of recursing, contradicting `docs/canon/field_resolution.md` invariant 12
//! ("addressing is independent of whether a node is cached or computed").
//!
//! These tests pin two things: the conversion behaves correctly, and it exists
//! in exactly one place so a fifth ingestion path cannot reintroduce the bug.

use beachcomber::provider::{MAX_JSON_DEPTH, SourceResult, Value};

/// One fixture exercised by every entry point below.
fn fixture() -> serde_json::Value {
    serde_json::json!({
        "flat": "top",
        "widget": {
            "kind": "renderable",
            "min_width": 8,
            "enabled": true,
            "nested": { "deep": "value" }
        },
        "items": ["a", "b"],
        "nothing": null,
        "ratio": 1.5
    })
}

fn assert_fixture_shape(r: &SourceResult) {
    assert_eq!(r.fields.get("flat"), Some(&Value::String("top".into())));
    assert_eq!(r.fields.get("ratio"), Some(&Value::Float(1.5)));
    assert_eq!(r.fields.get("nothing"), Some(&Value::String(String::new())));

    let widget = match r.fields.get("widget") {
        Some(Value::Object(m)) => m,
        other => panic!("widget must be an Object, got {other:?}"),
    };
    assert_eq!(
        widget.get("kind"),
        Some(&Value::String("renderable".into()))
    );
    assert_eq!(widget.get("min_width"), Some(&Value::Int(8)));
    assert_eq!(widget.get("enabled"), Some(&Value::Bool(true)));

    match widget.get("nested") {
        Some(Value::Object(m)) => {
            assert_eq!(m.get("deep"), Some(&Value::String("value".into())))
        }
        other => panic!("widget.nested must be an Object, got {other:?}"),
    }

    // Arrays become objects keyed by decimal index, so `items.0` addresses.
    match r.fields.get("items") {
        Some(Value::Object(m)) => {
            assert_eq!(m.get("0"), Some(&Value::String("a".into())));
            assert_eq!(m.get("1"), Some(&Value::String("b".into())));
        }
        other => panic!("items must be an Object, got {other:?}"),
    }
}

#[test]
fn nested_objects_survive_conversion() {
    assert_fixture_shape(&SourceResult::from_json_object(
        fixture().as_object().unwrap(),
    ));
}

#[test]
fn nested_values_are_addressable_by_path() {
    let r = SourceResult::from_json_object(fixture().as_object().unwrap());
    let pr = beachcomber::provider::ProviderResult { fields: r.fields };

    assert_eq!(
        pr.get_path("widget.nested.deep"),
        Some(&Value::String("value".into())),
        "invariant 12: a nested node must be addressable by dotted path"
    );
    assert_eq!(pr.get_path("items.1"), Some(&Value::String("b".into())));
    assert_eq!(pr.get_path("widget.absent"), None);
    assert_eq!(pr.get_path("flat.deeper"), None, "scalars have no subtree");
}

#[test]
fn depth_beyond_the_cap_is_stringified_not_dropped() {
    // Build an object nested one level deeper than the cap allows.
    let mut deepest = serde_json::json!({ "leaf": "bottom" });
    for _ in 0..=MAX_JSON_DEPTH {
        deepest = serde_json::json!({ "down": deepest });
    }

    let mut node = &Value::from_json(&deepest);
    let mut walked = 0usize;
    while let Value::Object(map) = node {
        match map.get("down") {
            Some(next) => {
                node = next;
                walked += 1;
            }
            None => break,
        }
    }

    assert!(
        walked < MAX_JSON_DEPTH + 1,
        "conversion must stop recursing at MAX_JSON_DEPTH, walked {walked}"
    );
    match node {
        Value::String(s) => assert!(
            s.contains("leaf"),
            "over-deep subtree is kept as JSON text, not dropped: {s}"
        ),
        other => panic!("expected a stringified subtree at the cap, got {other:?}"),
    }
}

#[test]
fn conversion_is_not_duplicated_across_ingestion_paths() {
    // The guard that makes the fix stick: pattern-matching serde_json::Value in
    // order to build a provider Value must happen in exactly one file. A new
    // ingestion path copying the old shape fails here rather than silently
    // shipping a fifth divergent conversion.
    let mut offenders = Vec::new();

    for entry in walk_rust_sources("src") {
        let text = std::fs::read_to_string(&entry).expect("read source");
        if text.contains("serde_json::Value::String(s) => Value::String(s.clone())")
            || text.contains(
                "serde_json::Value::String(s) => crate::provider::Value::String(s.clone())",
            )
        {
            offenders.push(entry);
        }
    }

    assert_eq!(
        offenders.len(),
        1,
        "JSON→Value conversion must live only in src/provider/mod.rs; found in {offenders:?}"
    );
    assert!(
        offenders[0].ends_with("provider/mod.rs"),
        "the one conversion must be Value::from_json, found in {:?}",
        offenders[0]
    );
}

fn walk_rust_sources(dir: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![std::path::PathBuf::from(dir)];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "rs") {
                out.push(p.to_string_lossy().into_owned());
            }
        }
    }
    out
}
