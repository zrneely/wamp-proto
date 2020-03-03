use failure::{Error, Fail};

use crate::{uri::Uri, Id, SessionScope};

/// An error produced by the WAMP crate directly.
#[derive(Debug, Fail)]
pub enum WampError {
    /// An operation timed out. The timeout value is configurable with `Client::set_timeout`.
    #[fail(display = "timed out")]
    Timeout,

    /// The router does not support a required role.
    #[fail(display = "router does not support required role {}", 0)]
    RouterSupportMissing(&'static str),

    /// The client is in an invalid state.
    #[fail(display = "the client is in an invalid state")]
    InvalidClientState,

    // The client transitioned to an invalid state while the request was running.
    #[fail(display = "the client transitioned to invalid state {}", 0)]
    ClientStateChanged(&'static str),

    #[fail(display = "router responded with error \"{}\"", error)]
    ErrorReceived {
        error: Uri,
        request_type: u64,
        request_id: Id<SessionScope>,
    },

    #[fail(display = "transport failed to connect: {}", 0)]
    ConnectFailed(Error),

    #[fail(
        display = "transport sink failed to become ready to send {}: {}",
        message_type, error
    )]
    WaitForReadyToSendFailed {
        message_type: &'static str,
        error: Error,
    },

    #[fail(
        display = "transport failed to send {} message: {}",
        message_type, error
    )]
    MessageSendFailed {
        message_type: &'static str,
        error: Error,
    },

    #[fail(
        display = "transport sink failed to flush after sending {}: {}",
        message_type, error
    )]
    SinkFlushFailed {
        message_type: &'static str,
        error: Error,
    },
}
