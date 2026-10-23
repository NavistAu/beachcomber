//! Source metadata for the external backends (http, library), built from a
//! `[providers.<p>.<s>]` table.

use crate::config::{ExternalFieldDecl, ExternalSourceConfig};
use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, SourceMetadata, SourceScope,
};

pub(crate) fn build_source_meta_from_external(cfg: &ExternalSourceConfig) -> SourceMetadata {
    let strategy_type = cfg.strategy_type.as_deref().unwrap_or("poll");

    let scope = match cfg.scope.as_deref() {
        Some("path") => SourceScope::PathScoped,
        _ => SourceScope::Global,
    };

    let poll_secs = cfg
        .poll_interval
        .as_ref()
        .and_then(|s| crate::scheduler::parse_duration_secs_pub(s))
        .unwrap_or(30);

    let poll_count = cfg.poll_count.unwrap_or(2);

    let patterns = cfg.fsevent_patterns.clone().unwrap_or_default();
    let abs_paths = cfg.fsevent_abs_paths.clone().unwrap_or_default();

    let invalidation = match strategy_type {
        "fsevent" => InvalidationStrategy::Watch {
            patterns: if scope == SourceScope::Global {
                vec![]
            } else {
                patterns.clone()
            },
            abs_paths: abs_paths.clone(),
        },
        "fsevent_poll" => InvalidationStrategy::WatchAndPoll {
            patterns: patterns.clone(),
            abs_paths: abs_paths.clone(),
            interval_secs: poll_secs,
            count: poll_count,
        },
        _ => InvalidationStrategy::Poll {
            interval_secs: poll_secs,
            count: poll_count,
        },
    };

    let fields = build_fields_from_decls(cfg.fields.as_deref());

    let failback_reattempts = cfg.failback_count.unwrap_or(3);
    let failback_secs = cfg
        .failback_interval
        .as_ref()
        .and_then(|s| crate::scheduler::parse_duration_secs_pub(s))
        .unwrap_or(60);

    SourceMetadata {
        name: cfg.name.clone(),
        fields,
        scope,
        invalidation,
        failback: FailbackConfig {
            reattempts: failback_reattempts,
            interval_secs: failback_secs,
        },
        fsevents_reinstate: cfg.fsevent_reinstates.unwrap_or(true),
        decay: None,
        budget_execute: crate::scheduler::lifecycle::DEFAULT_BUDGET_EXECUTE,
    }
}

fn build_fields_from_decls(decls: Option<&[ExternalFieldDecl]>) -> Vec<FieldSchema> {
    match decls {
        Some(d) if !d.is_empty() => d
            .iter()
            .map(|f| FieldSchema {
                name: f.name.clone(),
                field_type: match f.field_type.as_str() {
                    "int" => FieldType::Int,
                    "bool" => FieldType::Bool,
                    "float" => FieldType::Float,
                    _ => FieldType::String,
                },
            })
            .collect(),
        _ => vec![FieldSchema {
            name: "<field>".into(),
            field_type: FieldType::String,
        }],
    }
}
