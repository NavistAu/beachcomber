//! The validity hash: one function on both sides of the socket.
//!
//! FNV-1a, 64 bits, over a canonical encoding of its parts: each part is its
//! length, as eight little-endian bytes, then its bytes, so no two lists of
//! parts encode alike. The daemon hashes a Source record's stat inputs with
//! it ([`stamps_hash`]); a client's `cache()` hashes a memo's inputs with it.
//! The golden vectors in the tests pin it: a change to the function or to an
//! encoding changes every hash already stored, so it is not one to make.

use crate::fs::Stamp;
use std::path::Path;

const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a 64, fed parts.
#[derive(Clone, Copy, Debug)]
pub struct Hasher(u64);

impl Default for Hasher {
    fn default() -> Self {
        Self(OFFSET)
    }
}

impl Hasher {
    fn bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(PRIME);
        }
    }

    /// One part: its length, then its bytes.
    pub fn part(&mut self, part: &[u8]) -> &mut Self {
        self.bytes(&(part.len() as u64).to_le_bytes());
        self.bytes(part);
        self
    }

    pub fn finish(&self) -> u64 {
        self.0
    }
}

/// The hash of `parts`, in order.
pub fn hash<'a>(parts: impl IntoIterator<Item = &'a [u8]>) -> u64 {
    let mut h = Hasher::default();
    for part in parts {
        h.part(part);
    }
    h.finish()
}

/// The hash of stat inputs: each `(path, stamp)` in input order, the path's
/// bytes then the stamp's encoding ([`Stamp::encode`]).
pub fn stamps_hash<P: AsRef<Path>>(inputs: &[(P, Stamp)]) -> u64 {
    use std::os::unix::ffi::OsStrExt;
    let mut h = Hasher::default();
    for (path, stamp) in inputs {
        h.part(path.as_ref().as_os_str().as_bytes());
        h.part(&stamp.encode());
    }
    h.finish()
}

/// A hash as `:cache` shows it: 16 lowercase hex digits.
pub fn hex(hash: u64) -> String {
    format!("{hash:016x}")
}

/// What a check of stored inputs found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// Every input is as it was.
    Valid,
    /// An input changed.
    Invalid,
    /// An input could not be stamped, or lies where a stamp cannot be
    /// trusted: nothing is known, and what is stored is kept.
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::{Kind, Present};

    /// The golden vectors: these values are stored, so they never change.
    #[test]
    fn golden_vectors() {
        assert_eq!(
            hash([]),
            0xcbf2_9ce4_8422_2325,
            "no parts is the offset basis"
        );
        assert_eq!(Hasher::default().finish(), hash([]));
        assert_eq!(hex(hash([b"".as_slice()])), "a8c7f832281a39c5");
        assert_eq!(hex(hash([b"a".as_slice()])), "529a4ddc8ff56bbf");
        assert_eq!(hex(hash([b"ab".as_slice()])), "9c6001d328a94690");
        assert_eq!(
            hex(hash([b"a".as_slice(), b"b".as_slice()])),
            "41c80da72d0aec94"
        );
        let stamps = [
            ("/r/.git/HEAD", Stamp::Absent),
            ("/r/state", Stamp::Unknown),
            (
                "/r/f",
                Stamp::Present(Present {
                    dev: 1,
                    ino: 2,
                    size: 3,
                    mtime_secs: 1_700_000_000,
                    mtime_nanos: 5,
                    kind: Kind::File,
                    link: None,
                }),
            ),
        ];
        assert_eq!(hex(stamps_hash(&stamps)), "bfee800004d3b83b");
    }

    #[test]
    fn the_encoding_keeps_parts_apart() {
        assert_ne!(
            hash([b"ab".as_slice()]),
            hash([b"a".as_slice(), b"b".as_slice()])
        );
        assert_ne!(hash([b"".as_slice()]), hash([]));
    }

    #[test]
    fn a_hash_is_sixteen_hex_digits() {
        assert_eq!(hex(0), "0000000000000000");
        assert_eq!(hex(u64::MAX), "ffffffffffffffff");
    }
}
