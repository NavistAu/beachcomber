//! The root crate and `beachcomber-client` derive `BEACHCOMBER_VERSION`
//! independently at build time (see `build-common/version.rs`), each from its
//! own `build.rs`. This asserts the two derivations never disagree.

#[test]
fn root_and_client_report_the_same_version() {
    assert_eq!(env!("BEACHCOMBER_VERSION"), libbeachcomber::VERSION);
}
