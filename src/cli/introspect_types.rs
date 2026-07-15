/// Typed representation of the `introspect{daemon}` response payload.
///
/// Parsing into this struct once at the call site eliminates raw
/// `serde_json::Value` field lookups and provides compile-time enforcement
/// of the response shape.  If the protocol schema changes and removes a
/// required field, the `serde_json::from_value` call will return an error
/// at runtime instead of silently returning a default value.
///
/// Phase 11 of the interface-architecture roadmap: CLI becomes a pure
/// protocol consumer rather than a raw-JSON field accessor.
use serde::Deserialize;

/// A single health verdict emitted by a daemon introspect subject.
#[derive(Deserialize, Debug, Clone)]
pub struct Verdict {
    pub level: String,
    pub message: String,
}

impl Verdict {
    /// Map the verdict level to the exit-code severity used by `comb check`.
    /// FAIL → 2, WARN → 1, everything else → 0.
    pub fn severity(&self) -> u8 {
        match self.level.as_str() {
            "FAIL" => 2,
            "WARN" => 1,
            _ => 0,
        }
    }
}

/// Typed payload for `{"op":"introspect","subject":"daemon"}`.
///
/// Field names and types mirror the server-side serialisation in
/// `src/server.rs`.  Fields that can legitimately be absent from the
/// wire response are wrapped in `Option`.
#[derive(Deserialize, Debug)]
pub struct DaemonIntrospect {
    pub pid: i64,
    pub version: String,
    pub uptime_secs: u64,
    pub socket_path: String,
    pub config_path: Option<String>,
    pub requests_total: u64,
    pub in_flight: u64,
    pub active_watchers: u64,
    pub cache_entries: u64,
    /// "native", "polling", "disabled", or "unknown"; absent from pre-0.8 daemons.
    #[serde(default)]
    pub watch_backend: Option<String>,
    #[serde(default)]
    pub verdicts: Vec<Verdict>,
}
