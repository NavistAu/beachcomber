pub mod commands;
pub mod format;
pub mod introspect_types;
pub mod output_format;
pub mod status_format;

// Moved to `libbeachcomber` (Task 1.3) so non-CLI consumers can resolve
// fields in-process. Re-exported here so existing `crate::cli::path_expr::…`
// and `crate::cli::virtual_fields::…` call sites keep resolving; a later
// task migrates those call sites to import from `libbeachcomber` directly.
pub use libbeachcomber::path_expr;
pub use libbeachcomber::virtual_fields;
