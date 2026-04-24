use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");

    let cargo_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());

    let sha = Command::new("git")
        .args(["rev-parse", "--short=8", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());

    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);

    let tagged_clean = Command::new("git")
        .args(["describe", "--exact-match", "--tags", "HEAD"])
        .output()
        .ok()
        .map(|o| o.status.success())
        .unwrap_or(false)
        && !dirty;

    let version = if tagged_clean {
        cargo_version
    } else {
        match sha {
            Some(s) if dirty => format!("{cargo_version}+sha.{s}.dirty"),
            Some(s) => format!("{cargo_version}+sha.{s}"),
            None => format!("{cargo_version}+sha.unknown"),
        }
    };

    println!("cargo:rustc-env=BEACHCOMBER_VERSION={version}");
}
