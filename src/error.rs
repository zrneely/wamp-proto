
use std::time::Duration;

use proto;

/// An error produced by the WAMP crate directly.
#[derive(Debug, Fail)]
pub enum WampError {

    /// An operation timed out. The timeout value is configurable with `Client::set_timeout`.
    #[fail(display = "timed out after {:?}", time)]
    Timeout {
        time: Duration,
    },

    /// An unexpected message was received.
    #[fail(display = "unexpected message received: {:?} (was expecting {})", message, expecting)]
    UnexpectedMessage {
        message: proto::RxMessage,
        expecting: &'static str,
    },

    /// The transport's incoming message stream unexpectedly closed.
    #[fail(display = "transport unexpectedly closed")]
    TransportStreamClosed,
}
