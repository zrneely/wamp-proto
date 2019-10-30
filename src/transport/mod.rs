//! Contains the transport trait and supporting types, as well as first-party transport implementations.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

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
    async fn connect(url: &str, rv: Arc<MessageBuffer>) -> Result<Self, Error>;

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
    async fn close(self: Pin<&mut Self>) -> Result<(), Error>;
}

/// The types of value which can be sent over WAMP RPC and pub/sub boundaries.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TransportableValue {
    /// A non-negative integer.
    Integer(u64),
    /// A UTF-8 encoded string.
    String(String),
    /// A boolean value.
    Bool(bool),
    /// A list of other values.
    List(Vec<TransportableValue>),
    /// A string-to-value mapping.
    Dict(HashMap<String, TransportableValue>),
}
impl TransportableValue {
    /// Attempts to get the value, assuming it's an integer.
    pub fn into_int(self) -> Option<u64> {
        match self {
            TransportableValue::Integer(x) => Some(x),
            _ => None,
        }
    }

    /// Attempts to get the value, assuming it's a String.
    pub fn into_string(self) -> Option<String> {
        match self {
            TransportableValue::String(x) => Some(x),
            _ => None,
        }
    }

    /// Attempts to get the value, assuming it's a boolean.
    pub fn into_bool(self) -> Option<bool> {
        match self {
            TransportableValue::Bool(x) => Some(x),
            _ => None,
        }
    }

    /// Attempts to get the value, assuming it's a list.
    pub fn into_list(self) -> Option<Vec<TransportableValue>> {
        match self {
            TransportableValue::List(x) => Some(x),
            _ => None,
        }
    }

    /// Attempts to get the value, assuming it's a dictionary.
    pub fn into_dict(self) -> Option<HashMap<String, TransportableValue>> {
        match self {
            TransportableValue::Dict(x) => Some(x),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TransportableValue;

    #[test]
    fn transportable_value_test() {
        let tv = TransportableValue::Bool(true);
        assert_eq!(Some(true), tv.clone().into_bool());
        assert_eq!(None, tv.clone().into_dict());
        assert_eq!(None, tv.clone().into_int());
        assert_eq!(None, tv.clone().into_list());
        assert_eq!(None, tv.clone().into_string());

        let tv = TransportableValue::Dict(Default::default());
        assert_eq!(None, tv.clone().into_bool());
        assert_eq!(
            Some(std::collections::HashMap::new()),
            tv.clone().into_dict()
        );
        assert_eq!(None, tv.clone().into_int());
        assert_eq!(None, tv.clone().into_list());
        assert_eq!(None, tv.clone().into_string());

        let tv = TransportableValue::Integer(12345);
        assert_eq!(None, tv.clone().into_bool());
        assert_eq!(None, tv.clone().into_dict());
        assert_eq!(Some(12345), tv.clone().into_int());
        assert_eq!(None, tv.clone().into_list());
        assert_eq!(None, tv.clone().into_string());

        let tv = TransportableValue::List(vec![TransportableValue::Integer(12345)]);
        assert_eq!(None, tv.clone().into_bool());
        assert_eq!(None, tv.clone().into_dict());
        assert_eq!(None, tv.clone().into_int());
        assert_eq!(
            Some(vec![TransportableValue::Integer(12345)]),
            tv.clone().into_list()
        );
        assert_eq!(None, tv.clone().into_string());

        let tv = TransportableValue::String("asdf".into());
        assert_eq!(None, tv.clone().into_bool());
        assert_eq!(None, tv.clone().into_dict());
        assert_eq!(None, tv.clone().into_int());
        assert_eq!(None, tv.clone().into_list());
        assert_eq!(Some("asdf".into()), tv.clone().into_string());
    }
}
