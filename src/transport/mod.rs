//! Contains the transport trait and supporting types, as well as first-party transport implementations.

use std::collections::HashMap;

use async_trait::async_trait;
use failure::{Error, Fail};
use futures::{Sink, Stream};
use serde::{Deserialize, Serialize};

use crate::{rx::RxMessage, TxMessage};

/// A websocket-based transport.
#[cfg(feature = "ws_transport")]
pub mod websocket;

/// The different kinds of errors a transport could encounter.
#[derive(Debug, Fail)]
pub enum TransportError {
    /// The client will handle a ParseError by sending an ABORT message,
    /// and then exiting (according to section 5.3.3 of the WAMP spec).
    #[fail(display = "message parse error: {}", 0)]
    ParseError(Error),
    /// The client will simply immediately exit.
    #[fail(display = "network error: {}", 0)]
    NetworkError(Error),
}

/// A transport capable of supporting a WAMP connection.
///
/// While WAMP was designed with Websockets in mind, any message transport can be used if it is
/// message-based, bidirectional, ordered, and reliable. This crate includes one transport
/// implementation, the default websocket-based one, in [`transport::websocket::WebsocketTransport`].
///
/// A transport provides a [`Sink`] for [`TxMessage`] objects and a [`Stream`] for [`RxMessage`] objects,
/// which will be interpreted by [`Client`]s.
///
/// It is not the responsibility of the transport to associate meaning with any received messages.
/// For example, a transport should *not* close itself upon receiving an ABORT message. It should also
/// *not* look for or even be aware of out-of-order messages.
#[async_trait]
pub trait Transport {
    /// The type of Sink produced by this Transport. This must be named since GATs are not yet
    /// supported.
    type Sink: Sink<TxMessage, Error = Error> + Send + Unpin;
    /// The type of Stream produced by this Transport. This must be named since GATs are not yet
    /// supported.
    type Stream: Stream<Item = Result<RxMessage, TransportError>> + Send + Unpin;

    /// Asynchronously constructs a transport to the router at the given location.
    ///
    /// The format of the location string is implementation defined, but will probably be a URL.
    /// This method should do asynchronous work such as opening a TCP socket or initializing a
    /// websocket connection.
    ///
    /// # Return Value
    ///
    /// This method will eventually produce either a (Sink, Stream) tuple; or an error.
    ///
    /// # Panics
    ///
    /// This method may panic if not called under a tokio runtime. It should handle failures by returning
    /// [`Err`] from the appropriate future, or an Err() result for synchronous errors.
    async fn connect(url: &str) -> Result<(Self::Sink, Self::Stream), Error>;
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
    pub fn as_int(&self) -> Option<u64> {
        match self {
            TransportableValue::Integer(x) => Some(*x),
            _ => None,
        }
    }

    /// Attempts to get the value, assuming it's a String.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            TransportableValue::String(ref x) => Some(x.as_str()),
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
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            TransportableValue::Bool(x) => Some(*x),
            _ => None,
        }
    }

    /// Attempts to get the value, assuming it's a list.
    pub fn as_list(&self) -> Option<&Vec<TransportableValue>> {
        match self {
            TransportableValue::List(x) => Some(x),
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
    pub fn as_dict(&self) -> Option<&HashMap<String, TransportableValue>> {
        match self {
            TransportableValue::Dict(x) => Some(x),
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
#[allow(clippy::cognitive_complexity)]
mod tests {
    use super::TransportableValue;

    #[test]
    fn transportable_value_test() {
        let tv = TransportableValue::Bool(true);
        assert_eq!(Some(true), tv.as_bool());
        assert_eq!(None, tv.clone().into_dict());
        assert_eq!(None, tv.as_dict());
        assert_eq!(None, tv.as_int());
        assert_eq!(None, tv.clone().into_list());
        assert_eq!(None, tv.as_list());
        assert_eq!(None, tv.clone().into_string());
        assert_eq!(None, tv.as_str());

        let tv = TransportableValue::Dict(Default::default());
        assert_eq!(None, tv.as_bool());
        assert_eq!(
            Some(std::collections::HashMap::new()),
            tv.clone().into_dict()
        );
        assert!(tv.as_dict().unwrap().is_empty());
        assert_eq!(None, tv.as_int());
        assert_eq!(None, tv.clone().into_list());
        assert_eq!(None, tv.as_list());
        assert_eq!(None, tv.clone().into_string());
        assert_eq!(None, tv.as_str());

        let tv = TransportableValue::Integer(12345);
        assert_eq!(None, tv.as_bool());
        assert_eq!(None, tv.clone().into_dict());
        assert_eq!(None, tv.as_dict());
        assert_eq!(Some(12345), tv.as_int());
        assert_eq!(None, tv.clone().into_list());
        assert_eq!(None, tv.as_list());
        assert_eq!(None, tv.clone().into_string());
        assert_eq!(None, tv.as_str());

        let tv = TransportableValue::List(vec![TransportableValue::Integer(12345)]);
        assert_eq!(None, tv.as_bool());
        assert_eq!(None, tv.clone().into_dict());
        assert_eq!(None, tv.as_dict());
        assert_eq!(None, tv.as_int());
        assert_eq!(
            Some(vec![TransportableValue::Integer(12345)]),
            tv.clone().into_list()
        );
        assert_eq!(12345, tv.as_list().unwrap()[0].as_int().unwrap());
        assert_eq!(None, tv.into_string());

        let tv = TransportableValue::String("asdf".into());
        assert_eq!(None, tv.as_bool());
        assert_eq!(None, tv.clone().into_dict());
        assert_eq!(None, tv.as_dict());
        assert_eq!(None, tv.as_int());
        assert_eq!(None, tv.clone().into_list());
        assert_eq!(None, tv.as_list());
        assert_eq!(Some("asdf".into()), tv.clone().into_string());
        assert_eq!(Some("asdf"), tv.as_str());
    }
}
