//! Boundary traits — the seam between the daemon's pure logic and the
//! operating system. Every side-effecting call goes through one of these.

pub mod http;
pub mod killer;
pub mod library;
pub mod proc_snap;
pub mod process;
pub mod socket;
pub mod spawn;
