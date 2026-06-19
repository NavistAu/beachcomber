use beachcomber::provider::Provider;
use beachcomber::provider::gcloud::GcloudProvider;
use tempfile::TempDir;

fn make_gcloud_config(dir: &TempDir, active: &str, project: &str, account: &str) {
    // Create: <dir>/active_config  +  <dir>/configurations/config_<active>/properties
    std::fs::write(dir.path().join("active_config"), active).unwrap();
    let conf_dir = dir
        .path()
        .join("configurations")
        .join(format!("config_{active}"));
    std::fs::create_dir_all(&conf_dir).unwrap();
    let props = format!("[core]\nproject = {project}\naccount = {account}\n");
    std::fs::write(conf_dir.join("properties"), props).unwrap();
}

#[test]
fn gcloud_env_override_not_honored_in_daemon() {
    // Confirms CLOUDSDK_ACTIVE_CONFIG_NAME is NOT read by the daemon.
    // The daemon must use only the active_config file.
    let dir = TempDir::new().unwrap();
    make_gcloud_config(&dir, "default", "my-project", "me@example.com");

    // Set env var to a non-existent config — if daemon reads it, execute will return empty.
    let result = temp_env::with_vars(
        [
            ("CLOUDSDK_CONFIG", Some(dir.path().to_str().unwrap())),
            ("CLOUDSDK_ACTIVE_CONFIG_NAME", Some("nonexistent-config")),
        ],
        || {
            let sources = GcloudProvider.sources();
            sources[0].execute(None)
        },
    );
    // Daemon must use active_config file → "default" → project/account present.
    assert_eq!(
        result.fields.get("config_project").map(|v| v.as_text()),
        Some("my-project".to_string()),
        "daemon must follow active_config file, not $CLOUDSDK_ACTIVE_CONFIG_NAME"
    );
}
