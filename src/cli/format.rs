use serde_json;

/// Render a `-f fmt` template against a raw `serde_json::Value` (must be an Object).
///
/// Used by call sites that already hold a `serde_json::Value`.
///
/// The render itself is [`libbeachcomber::eval::render_template`] — the
/// workspace's one template render, sharing its environment with the resolver,
/// so `-f fmt`, `comb eval` and a `:template` facet leaf see the same filters
/// and the same undefined/`none` handling.
pub fn render_fmt_template_json(
    template: &str,
    data: &serde_json::Value,
) -> Result<String, String> {
    libbeachcomber::eval::render_template(template, data)
}
