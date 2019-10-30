use crate::{Id, SessionScope, Uri};

/// An error produced by the WAMP crate directly.
#[derive(Debug, Fail)]
pub enum WampError {
    /// An operation timed out. The timeout value is configurable with `Client::set_timeout`.
    #[fail(display = "timed out")]
    Timeout,

    /// An unexpected message was received.
    #[fail(
        display = "unexpected message received: {:?} (was expecting {})",
        message, expecting
    )]
    UnexpectedMessage {
        /// A textual representation of the unexpected message.
        message: String,
        /// The type of message expected.
        expecting: &'static str,
    },

    /// The transport's message stream was closed.
    #[fail(display = "transport stream closed")]
    TransportStreamClosed,

    /// The router does not support a required role.
    #[fail(display = "router does not support required role")]
    RouterSupportMissing,

    /// The client is in an invalid state.
    #[fail(display = "the client is in an invalid state")]
    InvalidClientState,

    #[fail(display = "router responded with error \"{}\"", error)]
    ErrorReceived {
        error: Uri,
        request_type: u64,
        request_id: Id<SessionScope>,
    },
}
