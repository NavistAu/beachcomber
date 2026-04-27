use beachcomber::proc_snapshot::decide_snapshot;
use std::collections::HashMap;

// ── decide_snapshot tests (platform-independent) ──────────────────────────

#[test]
fn empty_counts_yields_error() {
    let counts: HashMap<String, u64> = HashMap::new();
    let result = decide_snapshot(5, counts, "no processes");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "no processes");
}

#[test]
fn duration_secs_passed_through_to_summary() {
    let mut counts = HashMap::new();
    counts.insert("git".to_string(), 3);
    let result = decide_snapshot(42, counts, "no processes").unwrap();
    assert_eq!(result.duration_secs, 42);
}

#[test]
fn total_reflects_all_counts() {
    let mut counts = HashMap::new();
    counts.insert("git".to_string(), 5);
    counts.insert("myapp".to_string(), 2);
    let result = decide_snapshot(1, counts, "no processes").unwrap();
    assert_eq!(result.total, 7);
}

#[test]
fn known_category_produces_sample_with_category_field() {
    let mut counts = HashMap::new();
    counts.insert("git".to_string(), 3);
    let result = decide_snapshot(1, counts, "no processes").unwrap();
    let git_sample = result
        .samples
        .iter()
        .find(|s| s.command == "git")
        .expect("git sample missing");
    assert_eq!(git_sample.count, 3);
    assert_eq!(git_sample.category.as_deref(), Some("git"));
}

#[test]
fn uncategorized_process_produces_sample_without_category() {
    let mut counts = HashMap::new();
    counts.insert("myunknowntool".to_string(), 7);
    let result = decide_snapshot(1, counts, "no processes").unwrap();
    let sample = result
        .samples
        .iter()
        .find(|s| s.command == "myunknowntool")
        .expect("uncategorized sample missing");
    assert_eq!(sample.count, 7);
    assert!(sample.category.is_none());
}

#[test]
fn replacement_suggestions_emitted_for_covered_categories() {
    let mut counts = HashMap::new();
    counts.insert("kubectl".to_string(), 2);
    let result = decide_snapshot(1, counts, "no processes").unwrap();
    assert!(
        result
            .replacement_suggestions
            .iter()
            .any(|s| s.provider == "kubectl"),
        "expected a replacement suggestion for kubectl"
    );
}

#[test]
fn uncovered_category_yields_no_replacement_suggestion() {
    let mut counts = HashMap::new();
    counts.insert("node".to_string(), 4); // node category has covered=false
    let result = decide_snapshot(1, counts, "no processes").unwrap();
    assert!(
        result.replacement_suggestions.is_empty(),
        "node is not covered by beachcomber, no suggestion expected"
    );
}

#[test]
fn zero_count_category_does_not_appear_in_samples() {
    // Only insert an uncategorized entry so all CATEGORIES sum to 0.
    let mut counts = HashMap::new();
    counts.insert("myapp".to_string(), 1);
    let result = decide_snapshot(1, counts, "no processes").unwrap();
    for sample in &result.samples {
        assert!(
            sample.category.is_none(),
            "expected no categorized sample, got: {}",
            sample.command
        );
    }
}

// ── parse_eslogger_output tests (macOS only) ──────────────────────────────

#[cfg(target_os = "macos")]
mod eslogger {
    use beachcomber::proc_snapshot::parse_eslogger_output;

    #[test]
    fn empty_input_yields_empty_counts() {
        let counts = parse_eslogger_output("");
        assert!(counts.is_empty());
    }

    #[test]
    fn non_json_lines_are_skipped_not_fatal() {
        let input = "not json at all\nalso not json\n";
        let counts = parse_eslogger_output(input);
        assert!(counts.is_empty());
    }

    #[test]
    fn json_without_path_pointer_is_skipped() {
        // Valid JSON but neither path pointer present.
        let input = r#"{"some":"other","data":42}"#;
        let counts = parse_eslogger_output(input);
        assert!(counts.is_empty());
    }

    #[test]
    fn process_executable_path_pointer_parsed() {
        let line = r#"{"process":{"executable":{"path":"/usr/bin/git"}}}"#;
        let counts = parse_eslogger_output(line);
        assert_eq!(counts.get("git").copied(), Some(1));
    }

    #[test]
    fn event_exec_target_path_pointer_parsed() {
        let line = r#"{"event":{"exec":{"target":{"executable":{"path":"/usr/bin/kubectl"}}}}}"#;
        let counts = parse_eslogger_output(line);
        assert_eq!(counts.get("kubectl").copied(), Some(1));
    }

    #[test]
    fn process_pointer_takes_priority_over_event_pointer() {
        // Both pointers present — /process/executable/path wins (first `.or_else`).
        let line = r#"{"process":{"executable":{"path":"/usr/bin/git"}},"event":{"exec":{"target":{"executable":{"path":"/usr/bin/kubectl"}}}}}"#;
        let counts = parse_eslogger_output(line);
        assert_eq!(counts.get("git").copied(), Some(1));
        assert!(counts.get("kubectl").is_none());
    }

    #[test]
    fn basename_extracted_from_full_path() {
        let line = r#"{"process":{"executable":{"path":"/usr/local/bin/terraform"}}}"#;
        let counts = parse_eslogger_output(line);
        assert!(counts.contains_key("terraform"), "expected 'terraform' key");
        assert!(!counts.contains_key("/usr/local/bin/terraform"));
    }

    #[test]
    fn multiple_lines_counts_accumulate() {
        let input = concat!(
            r#"{"process":{"executable":{"path":"/usr/bin/git"}}}"#,
            "\n",
            r#"{"process":{"executable":{"path":"/usr/bin/git"}}}"#,
            "\n",
            r#"{"process":{"executable":{"path":"/usr/bin/mise"}}}"#,
            "\n",
        );
        let counts = parse_eslogger_output(input);
        assert_eq!(counts.get("git").copied(), Some(2));
        assert_eq!(counts.get("mise").copied(), Some(1));
    }

    #[test]
    fn malformed_line_mixed_with_valid_lines_does_not_stop_parsing() {
        let input = concat!(
            r#"{"process":{"executable":{"path":"/usr/bin/git"}}}"#,
            "\n",
            "this is garbage\n",
            r#"{"process":{"executable":{"path":"/usr/bin/aws"}}}"#,
            "\n",
        );
        let counts = parse_eslogger_output(input);
        assert_eq!(counts.get("git").copied(), Some(1));
        assert_eq!(counts.get("aws").copied(), Some(1));
    }

    /// End-to-end: parse_eslogger_output feeds into decide_snapshot correctly.
    #[test]
    fn parse_then_decide_produces_full_result() {
        let input = concat!(
            r#"{"process":{"executable":{"path":"/usr/bin/git"}}}"#,
            "\n",
            r#"{"process":{"executable":{"path":"/usr/bin/git"}}}"#,
            "\n",
        );
        let counts = parse_eslogger_output(input);
        let result = beachcomber::proc_snapshot::decide_snapshot(3, counts, "no events").unwrap();
        assert_eq!(result.duration_secs, 3);
        assert_eq!(result.total, 2);
        let git = result.samples.iter().find(|s| s.command == "git").unwrap();
        assert_eq!(git.count, 2);
        assert!(!result.replacement_suggestions.is_empty());
    }
}
