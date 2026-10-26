//! Seams between the daemon and the operating system: git, the process
//! table, sockets, plugin libraries and http. Most are traits a test
//! mocks; `http` is one function (`url()` calls it), since a test serves a
//! real local response instead. Not every side effect goes through one: the
//! built-in providers read files directly.

pub mod git;
pub mod http;
pub mod library;
pub mod proc_snap;
pub mod proc_table;
pub mod socket;
