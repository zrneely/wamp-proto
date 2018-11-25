
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
extern crate futures;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate log;
extern crate parking_lot;
extern crate rand;
extern crate regex;
extern crate serde;
extern crate tokio;

#[cfg(feature = "ws_transport")]
#[macro_use]
extern crate serde_json;
#[cfg(feature = "ws_transport")]
#[macro_use]
extern crate serde_derive;
#[cfg(feature = "ws_transport")]
extern crate websocket;

use std::collections::HashMap;
use std::fmt;
use std::marker::PhantomData;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use failure::Error;
use futures::prelude::*;
use parking_lot::Mutex;
use rand::{thread_rng, Rng};
use regex::Regex;

#[cfg(feature = "ws_transport")]
use serde::{
    de::{self, Deserialize, Deserializer, Visitor},
    Serialize,
    Serializer,
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

pub use client::*;

use pollable::PollableSet;
use proto::*;

// The maximum value of a WAMP ID.
const MAX_ID_VAL: u64 = 0x20_0000_0000_0000;

/// An RFC3989 URI.
///
/// These are used to identify topics, procedures, and errors in WAMP. A URI
/// consists of a number of "."-separated textual components. URI components must not contain
/// whitespace or the "#" character, and it is recommended that their components contain only
/// lower-case letters, digits, and "_". The first component of a WAMP URI must not be "wamp" -
/// that class of URI is reserved for protocol-level details. Empty components are permitted,
/// except in the first and last component.
///
/// An example of a well-formed URI is `"org.company.application.service"`.
#[derive(Debug, Clone, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "ws_transport", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "ws_transport", serde(deny_unknown_fields))]
pub struct Uri(String);
impl Uri {
    /// Constructs a URI from a textual representation, skipping all validation.
    ///
    /// It is highly recommended to use [`relaxed`] or [`strict`] instead, unless you are writing
    /// a transport implementation.
    pub fn raw(text: String) -> Self {
        Uri(text)
    }

    /// Constructs and validates a URI from a textual representation.
    ///
    /// Returns `None` if validation fails.
    pub fn relaxed(text: String) -> Option<Self> {
        lazy_static! {
            // regex taken from WAMP specification
            static ref RE: Regex = Regex::new(r"^([^\s\.#]+\.)*([^\s\.#]+)$").unwrap();
        }
        if RE.is_match(&text) && !text.starts_with("wamp.") {
            Some(Uri(text))
        } else {
            None
        }
    }

    /// Constructs and strictly validates a URI from a textual representation.
    ///
    /// Returns `None` if validation fails. A strict validation enforces that URI components only
    /// contain lower-case letters, digits, and "_".
    pub fn strict(text: String) -> Option<Self> {
        lazy_static! {
            // regex taken from WAMP specification
            static ref RE: Regex = Regex::new(r"^(([0-9a-z_]+\.)|\.)*([0-9a-z_]+)?$").unwrap();
        }
        if RE.is_match(&text) && !text.starts_with("wamp.") {
            Some(Uri(text))
        } else {
            None
        }
    }
}

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
pub struct Id<S> {
    value: u64,
    _pd: PhantomData<S>,
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
impl<S> Id<S> {
    /// Creates an ID from a raw `u64`. Should not be used except by [`Transport`] implementations.
    pub fn from_raw_value(value: u64) -> Self {
        Id { value, _pd: PhantomData }
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
            // critical anyway, so the tradeoff is not worth it IMO.
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
pub trait Transport: Sized + Sink<SinkItem = TxMessage, SinkError = Error> {
    /// The type of future returned when this transport opens a connection.
    type ConnectFuture: Future<Item = Self, Error = Error>;

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
    ///
    /// # Panics
    ///
    /// This method should never panic. It should handle failures by returning [`Err`] from the
    /// appropriate future, or an Err() result for synchronous errors.
    fn connect(url: &str) -> Result<ConnectResult<Self>, Error>;

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
    /// This method should never panic.
    fn listen(&mut self);
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
#[derive(Clone, Default)]
pub struct ReceivedValues {
    /// The queue of incoming "WELCOME" messages.
    pub welcome: TsPollSet<rx::Welcome>,
    /// The buffer of incoming "ABORT" messages.
    pub abort: TsPollSet<rx::Abort>,
    /// The buffer of incoming "GOODBYE" messages.
    pub goodbye: TsPollSet<rx::Goodbye>,
    /// The buffer of incoming "SUBSCRIBED" messages.
    pub subscribed: TsPollSet<rx::Subscribed>,

    /// A buffer of received errors. If an error is ever added, it is expected that
    /// no further messages are ever received.
    pub errors: Arc<Mutex<Option<Error>>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_raw_test() {
        assert_eq!("foo", Uri::raw("foo".into()).0);
        assert_eq!("~~~~1234_*()", Uri::raw("~~~~1234_*()".into()).0);
    }

    #[test]
    fn uri_relaxed_test() {
        let positive_tests = [
            "a.b.c.d123",
            "com.foobar.xyz~`$@!%^%",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "a,.b,c.d",
            "A.b.C.d",
        ];
        let negative_tests = [
            "wamp.f",
            "a.#.c.d",
            "..",
            "a..b.c.d",
            "a. .b.c.d",
            "a .b.c.d",
        ];

        for &test in positive_tests.iter() {
            println!("asserting that {} is a valid relaxed URI", test);
            assert!(Uri::relaxed(test.into()).is_some());
        }

        for &test in negative_tests.iter() {
            println!("asserting that {} is an invalid relaxed URI", test);
            assert!(Uri::relaxed(test.into()).is_none());
        }
    }

    #[test]
    fn uri_strict_test() {
        let positive_tests = [
            "a.b.c.d",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "a_.b_c.d123",
            "..",
            "a..b.c.d",
        ];
        let negative_tests = [
            "wamp.f",
            "com.foobar.xyz~`$@!%^%",
            "A.b.C.d",
            "a.#.c.d",
            "a. .b.c.d",
            "a .b.c.d",
            "45.$$$",
            "-=-=-=-=-",
        ];

        for &test in positive_tests.iter() {
            println!("asserting that {} is a valid strict URI", test);
            assert!(Uri::strict(test.into()).is_some());
        }

        for &test in negative_tests.iter() {
            println!("asserting that {} is an invalid strict URI", test);
            assert!(Uri::strict(test.into()).is_none());
        }
    }

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
        assert_eq!(12345, serde_json::from_value::<Id<SessionScope>>(json!(12345)).unwrap().value);
        assert_eq!(12345, serde_json::from_value::<Id<RouterScope>>(json!(12345)).unwrap().value);
        assert_eq!(12345, serde_json::from_value::<Id<GlobalScope>>(json!(12345)).unwrap().value);

        assert_eq!(MAX_ID_VAL, serde_json::from_value::<Id<SessionScope>>(json!(MAX_ID_VAL)).unwrap().value);
        assert_eq!(MAX_ID_VAL, serde_json::from_value::<Id<RouterScope>>(json!(MAX_ID_VAL)).unwrap().value);
        assert_eq!(MAX_ID_VAL, serde_json::from_value::<Id<GlobalScope>>(json!(MAX_ID_VAL)).unwrap().value);

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
        assert_eq!(Some(vec![TransportableValue::Integer(12345)]), tv.clone().into_list());
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
        let uri = Uri::strict("a.b.c.d".into()).unwrap();

        let value = serde_json::to_value(uri).unwrap();
        assert_eq!(json!("a.b.c.d"), value);
    }

    #[cfg(feature = "ws_transport")]
    #[test]
    fn uri_deserialization_test() {
        let value = json!("a.b.c.d");
        assert_eq!(Uri::raw("a.b.c.d".into()), serde_json::from_value(value).unwrap());

        let value = json!(["a", 1]);
        assert!(serde_json::from_value::<Uri>(value).is_err());

        let value = json!(1);
        assert!(serde_json::from_value::<Uri>(value).is_err());
    }
}