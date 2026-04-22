#[test]
fn minijinja_renders_basic_template() {
    let env = minijinja::Environment::new();
    let out = env
        .render_str("Hello {{ name }}", minijinja::context!(name => "world"))
        .unwrap();
    assert_eq!(out, "Hello world");
}

#[test]
fn minijinja_upper_filter_available() {
    let env = minijinja::Environment::new();
    let out = env
        .render_str("{{ sha | upper }}", minijinja::context!(sha => "abcdef12"))
        .unwrap();
    assert_eq!(out, "ABCDEF12");
}

#[test]
fn minijinja_default_filter_available() {
    let env = minijinja::Environment::new();
    let out = env
        .render_str("{{ missing | default('fallback') }}", minijinja::context!())
        .unwrap();
    assert_eq!(out, "fallback");
}
