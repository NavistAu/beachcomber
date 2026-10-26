use std::path::Path;

use beachcomber::client::Client;
use libbeachcomber::resolve::{Fetch, FetchError, Got};

/// One daemon `get` the way the CLI does it: a fresh session on `sock`, then
/// `Fetch::get(key, path, force = false, wait = Wait::Budget)`.
pub fn fetch(sock: &Path, key: &str, path: Option<&str>) -> Result<Option<Got>, FetchError> {
    let mut session = Client::new(sock.to_path_buf())
        .connect()
        .map_err(FetchError::Transport)?;
    Fetch::get(&mut session, key, path, false, libbeachcomber::Wait::Budget)
}
