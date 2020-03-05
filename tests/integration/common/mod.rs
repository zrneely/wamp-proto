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

struct Tee<T: tokio::io::AsyncRead + Unpin> {
    name: &'static str,
    inner: tokio::io::Lines<tokio::io::BufReader<T>>,
}
impl<T: tokio::io::AsyncRead + Unpin> Tee<T> {
    async fn next_line(&mut self) -> tokio::io::Result<Option<String>> {
        match self.inner.next_line().await {
            Ok(Some(text)) => {
                println!("{}: {}", self.name, text);
                Ok(Some(text))
            }
            t => t,
        }
    }
}
