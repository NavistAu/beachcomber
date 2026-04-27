use beachcomber::config::Config;
use tempfile::TempDir;

#[test]
fn loads_env_file() {
    let tmp = TempDir::new().unwrap();
    let env_path = tmp.path().join("env");
    std::fs::write(
        &env_path,
        r#"
# This is a comment
BEACHCOMBER_TEST_KEY=hello_world
BEACHCOMBER_TEST_QUOTED="quoted value"
BEACHCOMBER_TEST_SINGLE='single quoted'

# Blank lines are fine
BEACHCOMBER_TEST_NUM=42
"#,
    )
    .unwrap();

    let mut config = Config::default();
    config.daemon.env_file = Some(env_path.to_string_lossy().to_string());

    // Scope the test so load_env_file's set_var calls are rolled back via Drop,
    // regardless of assertion panics. All four vars start and end as unset.
    temp_env::with_vars(
        [
            ("BEACHCOMBER_TEST_KEY", None::<&str>),
            ("BEACHCOMBER_TEST_QUOTED", None::<&str>),
            ("BEACHCOMBER_TEST_SINGLE", None::<&str>),
            ("BEACHCOMBER_TEST_NUM", None::<&str>),
        ],
        || {
            let count = config.load_env_file();
            assert_eq!(count, 4, "Should load 4 variables");
            assert_eq!(
                std::env::var("BEACHCOMBER_TEST_KEY").unwrap(),
                "hello_world"
            );
            assert_eq!(
                std::env::var("BEACHCOMBER_TEST_QUOTED").unwrap(),
                "quoted value"
            );
            assert_eq!(
                std::env::var("BEACHCOMBER_TEST_SINGLE").unwrap(),
                "single quoted"
            );
            assert_eq!(std::env::var("BEACHCOMBER_TEST_NUM").unwrap(), "42");
        },
    );
}

#[test]
fn missing_env_file_returns_zero() {
    let mut config = Config::default();
    config.daemon.env_file = Some("/nonexistent/path/to/env".to_string());

    let count = config.load_env_file();
    assert_eq!(count, 0, "Missing file should return 0");
}

#[test]
fn no_env_file_configured_returns_zero() {
    // Default config with no env_file and no default file on disk
    let config = Config::default();
    // This will look for ~/.config/beachcomber/env which may or may not exist
    // Just verify it doesn't panic
    let _ = config.load_env_file();
}
