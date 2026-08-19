use minijinja::Environment;

/// Build a minijinja `Environment` with the shared custom filters pre-registered.
///
/// Shared by all rendering helpers so the `truncate` filter (and any future
/// additions) are available in `fmt`, `eval`, and any other template surface.
pub fn build_env<'a>() -> Environment<'a> {
    let mut env = Environment::new();
    env.add_filter("truncate", truncate_filter);
    env.add_filter("basename", basename_filter);
    env
}

fn basename_filter(value: String) -> String {
    // Remove trailing slashes, then take the last path component.
    let trimmed = value.trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    trimmed.rsplit('/').next().unwrap_or(trimmed).to_string()
}

fn truncate_filter(value: String, length: u32) -> String {
    if value.chars().count() <= length as usize {
        value
    } else {
        let mut s: String = value.chars().take(length as usize).collect();
        s.push_str("...");
        s
    }
}
