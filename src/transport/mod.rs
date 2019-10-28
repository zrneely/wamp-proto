//! Contains first-party transport implementations.

use async_trait::async_trait;
use failure::Error;
use tokio::prelude::*;

use crate::{MessageBuffer, TxMessage};

/// A websocket-based transport.
#[cfg(feature = "ws_transport")]
pub mod websocket;

/// A transport capable of supporting a WAMP connection.
///
/// While WAMP was designed with Websockets in mind, any message transport can be used if it is
/// message-based, bidirectional, ordered, and reliable. This crate includes one transport
/// implementation, the default websocket-based one, in [`transport::websocket::WebsocketTransport`].
///
/// A transport provides a [`Sink`] for [`TxMessage`] objects. It also provides what is effectively
/// a [`Stream`] for each type of incoming message.
///
/// It is not the responsibility of the transport to associate meaning with any received messages.
/// For example, a transport should *not* close itself upon receiving an ABORT message. It should also
/// *not* look for or even be aware of out-of-order messages.
///
/// If a transport's underlying connection fatally fails or is lost, transports should inform their
/// client by adding to the "errors" list in the MessageBuffer. Transport implementations should not
/// attempt to transparently recreate the connection. The WAMP protocol defines a WAMP session's
/// lifetime as a subset of the underlying connection's lifetime, so the WAMP session will have to be
/// re-established in that case.
///
/// If a message is pushed to the `transport_errors` queue in [`MessageBuffer`], `close()` will *NOT*
/// be called on the transport. The transport should clean up all of its resources independently in
/// that scenario.
#[async_trait]
pub trait Transport: Sized + Sink<TxMessage, Error = Error> + Send {
    /// Asynchronously constructs a transport to the router at the given location.
    ///
    /// The format of the location string is implementation defined, but will probably be a URL.
    /// This method should do asynchronous work such as opening a TCP socket or initializing a
    /// websocket connection.
    ///
    /// # Return Value
    ///
    /// This method returns a future which, when resolved, provides the actual transport instance.
    ///
    /// # Panics
    ///
    /// This method may panic if not called under a tokio runtime. It should handle failures by returning
    /// [`Err`] from the appropriate future, or an Err() result for synchronous errors.
    async fn connect(url: &str, rv: MessageBuffer) -> Result<Self, Error>;

    /// Spawns a long-running task which will listen for events and forward them to the
    /// [`MessageBuffer`] returned by the [`connect`] method. Multiple calls to this method
    /// have no effect beyond the first. This method must be called under a Tokio reactor.
    ///
    /// # Remarks
    ///
    /// It is unfortunately necessary to have a long-running task, not just to handle RPC
    /// invokations, but also to act as an underlying data source for the [`PollableSet`]s which
    /// the client queries. It is critical that this buffer is used so that messages can arrive
    /// out-of-order and client [`Future`]s will still be properly notified, without being stuck
    /// in a notify loop.
    ///
    /// # Panics
    ///
    /// This method will panic if not called under a tokio runtime.
    fn listen(&mut self);

    /// Closes whatever connection this has open and stops listening for incoming messages.
    /// Calling this before `listen` is undefined behavior.
    async fn close(&mut self) -> Result<(), Error>;
}
