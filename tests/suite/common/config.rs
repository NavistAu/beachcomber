/// `body`, a config file's text, stamped at this comb's schema
/// (`schema.version`) as `comb migrate` stamps it. An unstamped file is
/// migrated on every load that reads it: a lock, a `.bak`, a rewrite and a
/// record, which a test about something else need not pay. A test about
/// migration writes its files as they are.
pub fn stamped(body: &str) -> String {
    use beachcomber::config::migrate::{current, stamp};
    stamp(body, current())
}
