use beachcomber::provider::Provider;
#[test]
fn non_file_sources_watch_no_explicit_files() {
    for s in beachcomber::provider::git::GitProvider.sources() {
        assert!(s.watched_files(Some("/tmp")).is_empty());
    }
}
