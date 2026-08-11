//! The 3.3.5a logon server: SRP6 authentication and the realm list.
//!
//! Written from the public protocol documentation. This is a small, strictly
//! ordered conversation on TCP port 3724 -- challenge, proof, realm list -- and
//! it produces the session key the world server needs, so it has to come first.

pub mod client;
pub mod protocol;
pub mod srp6;

pub use client::{login, LoggedIn};
pub use protocol::{AuthResult, Realm};
pub use srp6::{Session, SESSION_KEY_LEN};
