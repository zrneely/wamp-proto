
use std::time::Duration;

/// An error produced by the WAMP crate directly.
#[derive(Debug, Fail)]
pub enum WampError {

    /// An operation timed out. The timeout value is configurable with `Client::set_timeout`.
    #[fail(display = "timed out after {:?}", time)]
    Timeout {
        /// The length of the timeout that expired.
        time: Duration,
    },

    /// An unexpected message was received.
    #[fail(display = "unexpected message received: {:?} (was expecting {})", message, expecting)]
    UnexpectedMessage {
        /// A textual representation of the unexpected message.
        message: String,
        /// The type of message expected.
        expecting: &'static str,
    },

    /// The transport's incoming message stream unexpectedly closed.
    #[fail(display = "transport unexpectedly closed")]
    TransportStreamClosed,

    /// The router does not support a required role.
    #[fail(display = "router does not support required role")]
    RouterSupportMissing,
}
