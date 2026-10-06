use std::path::Path;

use beachcomber::client::Client;
use libbeachcomber::resolve::{Fetch, FetchError, Got};

/// One daemon `get` the way the CLI does it: a fresh `ClientSession` on
/// `sock`, then `Fetch::get(key, path, force = false, wait = false)`.
pub fn fetch(sock: &Path, key: &str, path: Option<&str>) -> Result<Option<Got>, FetchError> {
    let mut session = Client::new(sock.to_path_buf())
        .connect()
        .map_err(|e| FetchError::Transport(e.to_string()))?;
    session.get(key, path, false, false)
}
