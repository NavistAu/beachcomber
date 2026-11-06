//! A test's client: one copy for every module, so a choice such as how long
//! to keep connecting is made once.

use libbeachcomber::{Client, ClientConfig};
use std::path::Path;
use std::time::Duration;

/// A client of the daemon at `sock`: no autostart, no `exec()`, two seconds
/// for a read or a write, and the library's connect timeout. A test of a
/// socket nobody serves adds `.with_connect_timeout(Duration::ZERO)`, one
/// attempt, rather than wait out the default's retries.
pub fn client_for(sock: &Path) -> Client {
    Client::with_config(ClientConfig {
        timeout: Duration::from_secs(2),
        auto_start: false,
        exec: false,
    })
    .with_socket_path(sock.to_path_buf())
}
