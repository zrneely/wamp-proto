//! A futures-based WAMP client implementation, capable of taking the roles caller, callee,
//! publisher, and subscriber.
//!
//! It is extensible to any suitable transport. Transport implementation are provided via
//! other crates.
//!
//! This crate implements the WAMP basic profile, and (for now) *none* of the advanced profile.

#![deny(missing_docs)]

extern crate failure;
#[macro_use]
extern crate failure_derive;
#[macro_use]
extern crate futures;
extern crate http;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate log;
extern crate parking_lot;
extern crate rand;
extern crate regex;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate tokio;

#[cfg(feature = "ws_transport")]
#[macro_use]
extern crate serde_json;
#[cfg(feature = "ws_transport")]
extern crate websocket;

use std::collections::HashMap;
use std::fmt;
use std::marker::PhantomData;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use failure::Error;
use futures::prelude::*;
use parking_lot::Mutex;
use rand::{thread_rng, Rng};

#[cfg(feature = "ws_transport")]
use serde::{
    de::{self, Deserialize, Deserializer, Visitor},
    Serialize, Serializer,
};

/// Contains protocol-level details.
///
/// If you aren't defining your own transport type, you shouldn't need to worry about this module.
pub mod proto;

/// Contains [`Transport`] implementations.
pub mod transport;

/// Useful types for [`Transport`] implementations.
pub mod pollable;

mod client;
mod error;
mod uri;

pub use client::*;
pub use uri::*;

use pollable::PollableSet;
use proto::*;

// The maximum value of a WAMP ID.
const MAX_ID_VAL: u64 = 0x20_0000_0000_0000;

/// [`ID`]s in the global scope are chosen randomly from a uniform distribution over the possible
/// values of an [`ID`].
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum GlobalScope {}

/// [`ID`]s in the router scope are chosen by the router using an arbitrary algorithm.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum RouterScope {}

/// [`ID`]s in the session scope are incremented by 1, from 1.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum SessionScope {}

/// A WAMP ID is used to identify sessions, publications, subscriptions, registrations, and
/// requests.
///
/// WAMP IDs are internally integers between 0 and 2^53 (inclusive).
///
/// The type parameter should be one of [`GlobalScope`], [`RouterScope`], or [`SessionScope`]. It
/// is a compile time-only value which describes the scope of the ID.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct Id<Scope> {
    value: u64,
    _pd: PhantomData<Scope>,
}
impl<Scope> Serialize for Id<Scope> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(self.value)
    }
}
impl<'de, Scope> Deserialize<'de> for Id<Scope> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Id {
            value: deserializer.deserialize_u64(ScopeVisitor)?,
            _pd: PhantomData,
        })
    }
}
struct ScopeVisitor;
impl<'de> Visitor<'de> for ScopeVisitor {
    type Value = u64;
    fn expecting(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.write_str("an integer between 0 and 2^53 (inclusive)")
    }

    fn visit_u8<E: de::Error>(self, value: u8) -> Result<u64, E> {
        Ok(value as u64)
    }

    fn visit_u16<E: de::Error>(self, value: u16) -> Result<u64, E> {
        Ok(value as u64)
    }

    fn visit_u32<E: de::Error>(self, value: u32) -> Result<u64, E> {
        Ok(value as u64)
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<u64, E> {
        if value > MAX_ID_VAL {
            Err(E::custom(format!("u64 out of range: {}", value)))
        } else {
            Ok(value as u64)
        }
    }
}
impl<Scope> Id<Scope> {
    /// Creates an ID from a raw `u64`. Should not be used except by [`Transport`] implementations.
    pub fn from_raw_value(value: u64) -> Self {
        Id {
            value,
            _pd: PhantomData,
        }
    }
}
impl Id<GlobalScope> {
    /// Generates a random global ID.
    pub fn generate() -> Self {
        // TODO: If rand::distributions::Range::new ever becomes const, make the range const - this
        // could significantly improve the performance of this method.
        Id {
            value: thread_rng().gen_range(0, MAX_ID_VAL + 1),
            _pd: PhantomData,
        }
    }
}
impl Id<SessionScope> {
    /// Generates a sequential ID.
    ///
    /// Note that this will overflow and silently wrap around to 0 after exhausting
    /// [`std::usize::max_value`] IDs. This method is thread-safe.
    pub fn next() -> Self {
        lazy_static! {
            static ref NEXT: AtomicUsize = AtomicUsize::new(1);
        }
        Id {
            // This could maybe be Ordering::Relaxed, but I'm not sure and I don't want
            // to risk it. This section of code is probably not particularly performance-
            // critical anyway, so the tradeoff is not worth it.
            value: NEXT.fetch_add(1, Ordering::SeqCst) as u64,
            _pd: PhantomData,
        }
    }
}
// We deliberately do not implement a producer for Id<RouterScope> - they're ALWAYS
// generated by the router, we just store/send them (parsing/serializing is handled by
// the transport).

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
/// client by adding to the "errors" list in the ReceivedValues. Transport implementations should not
/// attempt to transparently recreate the connection. The WAMP protocol defines a WAMP session's
/// lifetime as a subset of the underlying connection's lifetime, so the WAMP session will have to be
/// re-established in that case.
/// 
/// If a message is pushed to the `transport_errors` queue in [`ReceivedValues`], `close()` will *NOT*
/// be called on the transport. The transport should clean up all of its resources independently in
/// that scenario.
pub trait Transport: Sized + Sink<SinkItem = TxMessage, SinkError = Error> + Send {
    /// The type of future returned when this transport opens a connection.
    type ConnectFuture: Future<Item = Self, Error = Error> + Send;
    /// The type of future returned when this transport is closed.
    type CloseFuture: Future<Item = (), Error = Error> + Send;

    /// Asynchronously constructs a transport to the router at the given location. This method
    /// must be called under a Tokio runtime.
    ///
    /// The format of the location string is implementation defined, but will probably be a URL.
    /// This method should do asynchronous work such as opening a TCP socket or initializing a
    /// websocket connection.
    ///
    /// # Return Value
    ///
    /// This method returns a future which, when resolved, provides the actual transport instance.
    /// It also returns a shared [`ReceivedValues`].
    /// TODO: the client should create the ReceivedValues dictionary and pass it into connect().
    ///
    /// # Panics
    ///
    /// This method may panic if not called under a tokio runtime. It should handle failures by returning
    /// [`Err`] from the appropriate future, or an Err() result for synchronous errors.
    fn connect(url: &str) -> ConnectResult<Self>;

    /// Spawns a long-running task which will listen for events and forward them to the
    /// [`ReceivedValues`] returned by the [`connect`] method. Multiple calls to this method
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
    fn close(&mut self) -> Self::CloseFuture;
}

/// The result of connecting to a channel.
pub struct ConnectResult<T: Transport> {
    /// The task which will eventually provide the actual Transport object.
    pub future: T::ConnectFuture,
    /// The queue of incoming messages and outgoing commands to the message handling task.
    pub received_values: ReceivedValues,
}

/// A thread-safe pollable set - convenience typedef to save on typing. This effectively acts
/// as an inefficient spmc queue, with the ability to listen only for messages that pass a
/// caller-defined predicate.
// TODO use hashmap of request ID -> oneshot spmc queues instead? less locking, probably more
// efficient
pub type TsPollSet<T> = Arc<Mutex<PollableSet<T>>>;

/// Acts as a buffer for each type of returned message, and any possible errors
/// (either in the transport layer or the protocol).
#[derive(Clone, Default, Debug)]
pub struct ReceivedValues {
    /// The queue of incoming "WELCOME" messages.
    pub welcome: TsPollSet<rx::Welcome>,
    /// The buffer of incoming "ABORT" messages.
    pub abort: TsPollSet<rx::Abort>,
    /// The buffer of incoming "GOODBYE" messages.
    pub goodbye: TsPollSet<rx::Goodbye>,
    /// The buffer of incoming "ERROR" messages.
    pub error: TsPollSet<rx::Error>,
    /// The buffer of incoming fatal transport issues.
    pub transport_errors: TsPollSet<Error>,

    /// The buffer of incoming "SUBSCRIBED" messages.
    #[cfg(feature = "subscriber")]
    pub subscribed: TsPollSet<rx::Subscribed>,
    /// The buffer of incoming "UNSUBSCRIBED" messages.
    #[cfg(feature = "subscriber")]
    pub unsubscribed: TsPollSet<rx::Unsubscribed>,
    /// The buffer of incoming "EVENT" messages.
    #[cfg(feature = "subscriber")]
    pub event: TsPollSet<rx::Event>,

    /// The buffer of incoming "PUBLISHED" messages.
    #[cfg(feature = "publisher")]
    pub published: TsPollSet<rx::Published>,

    /// The buffer of incoming "REGISTERED" messages.
    #[cfg(feature = "callee")]
    pub registered: TsPollSet<rx::Registered>,
    /// The buffer of incoming "UNREGISTERED" messages.
    #[cfg(feature = "callee")]
    pub unregistered: TsPollSet<rx::Unregistered>,
    /// The buffer of incoming "INVOCATION" messages.
    #[cfg(feature = "callee")]
    pub invocation: TsPollSet<rx::Invocation>,

    /// The buffer of incoming "RESULT" messages.
    #[cfg(feature = "caller")]
    pub result: TsPollSet<rx::Result>,
}
impl ReceivedValues {
    #[cfg(test)]
    fn len(&self) -> usize {
        let mut len = self.welcome.lock().len()
            + self.abort.lock().len()
            + self.goodbye.lock().len()
            + self.error.lock().len();

        #[cfg(feature = "subscriber")]
        {
            len += self.subscribed.lock().len()
                + self.unsubscribed.lock().len()
                + self.event.lock().len();
        }

        #[cfg(feature = "publisher")]
        {
            len += self.published.lock().len();
        }

        #[cfg(feature = "callee")]
        {
            len += self.registered.lock().len()
                + self.unregistered.lock().len()
                + self.invocation.lock().len();
        }

        #[cfg(feature = "caller")]
        {
            len += self.result.lock().len();
        }

        len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_global_test() {
        let id1 = Id::<GlobalScope>::generate();
        let id2 = Id::<GlobalScope>::generate();
        assert!(id1 != id2);

        let id3 = id1.clone();
        assert_eq!(id1, id3);
    }

    #[test]
    fn id_router_test() {
        let id1: Id<RouterScope> = Id {
            value: 1234,
            _pd: PhantomData,
        };
        let id2: Id<RouterScope> = Id {
            value: 12345,
            _pd: PhantomData,
        };
        let id3 = id1.clone();

        assert!(id1 != id2);
        assert_eq!(id1, id3);
    }

    #[test]
    fn id_session_test() {
        let id1 = Id::<SessionScope>::next();
        let id2 = Id::<SessionScope>::next();
        assert!(id1 != id2);

        let id3 = id1.clone();
        assert_eq!(id1, id3);
    }

    #[cfg(feature = "ws_transport")]
    #[test]
    fn id_serialization_test() {
        let id = Id::<GlobalScope>::generate();
        let id_val = id.value;
        assert_eq!(json!(id_val), serde_json::to_value(id).unwrap());

        let id: Id<RouterScope> = Id {
            value: 1234,
            _pd: PhantomData,
        };
        assert_eq!(json!(1234), serde_json::to_value(id).unwrap());

        let id = Id::<SessionScope>::next();
        let id_val = id.value;
        assert_eq!(json!(id_val), serde_json::to_value(id).unwrap());
    }

    #[cfg(feature = "ws_transport")]
    #[test]
    fn id_deserialization_test() {
        assert_eq!(
            12345,
            serde_json::from_value::<Id<SessionScope>>(json!(12345))
                .unwrap()
                .value
        );
        assert_eq!(
            12345,
            serde_json::from_value::<Id<RouterScope>>(json!(12345))
                .unwrap()
                .value
        );
        assert_eq!(
            12345,
            serde_json::from_value::<Id<GlobalScope>>(json!(12345))
                .unwrap()
                .value
        );

        assert_eq!(
            MAX_ID_VAL,
            serde_json::from_value::<Id<SessionScope>>(json!(MAX_ID_VAL))
                .unwrap()
                .value
        );
        assert_eq!(
            MAX_ID_VAL,
            serde_json::from_value::<Id<RouterScope>>(json!(MAX_ID_VAL))
                .unwrap()
                .value
        );
        assert_eq!(
            MAX_ID_VAL,
            serde_json::from_value::<Id<GlobalScope>>(json!(MAX_ID_VAL))
                .unwrap()
                .value
        );

        assert!(serde_json::from_value::<Id<SessionScope>>(json!(MAX_ID_VAL + 1)).is_err());
        assert!(serde_json::from_value::<Id<RouterScope>>(json!(MAX_ID_VAL + 1)).is_err());
        assert!(serde_json::from_value::<Id<GlobalScope>>(json!(MAX_ID_VAL + 1)).is_err());

        assert!(serde_json::from_value::<Id<SessionScope>>(json!(-1)).is_err());
        assert!(serde_json::from_value::<Id<RouterScope>>(json!(-1)).is_err());
        assert!(serde_json::from_value::<Id<GlobalScope>>(json!(-1)).is_err());
    }

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
        assert_eq!(Some(HashMap::new()), tv.clone().into_dict());
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

    #[cfg(feature = "ws_transport")]
    #[test]
    fn uri_serialization_test() {
        let uri = Uri::strict("a.b.c.d").unwrap();

        let value = serde_json::to_value(uri).unwrap();
        assert_eq!(json!("a.b.c.d"), value);
    }

    #[cfg(feature = "ws_transport")]
    #[test]
    fn uri_deserialization_test() {
        let value = json!("a.b.c.d");
        assert_eq!(
            Uri::raw("a.b.c.d".into()),
            serde_json::from_value::<Uri>(value).unwrap()
        );

        let value = json!(["a", 1]);
        assert!(serde_json::from_value::<Uri>(value).is_err());

        let value = json!(1);
        assert!(serde_json::from_value::<Uri>(value).is_err());
    }
}
