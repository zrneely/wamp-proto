//! Helpers and common code for integration tests.
//! This assumes several things about the current environment:
//!
//! * `crossbar` is installed and available on the `PATH`.
//! * `nodejs` is installed and available on the `PATH`.

mod peer;
mod router;

pub const TEST_REALM: &str = "wamp_proto_test";

pub use peer::*;
pub use router::*;
