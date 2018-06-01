
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
extern crate tokio_core;

#[cfg(feature = "ws_transport")]
#[macro_use]
extern crate serde_json;
#[cfg(feature = "ws_transport")]
#[macro_use]
extern crate serde_derive;
#[cfg(feature = "ws_transport")]
extern crate websocket;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering}
};
use std::collections::HashMap;
use std::marker::PhantomData;

use failure::Error;
use futures::prelude::*;
use parking_lot::Mutex;
use rand::{thread_rng, Rng};
use regex::Regex;
use tokio_core::reactor;

/// Contains protocol-level details.
///
/// If you aren't defining your own transport type, you shouldn't need to worry about this module.
pub mod proto;
/// Contains `Transport` implementations.
pub mod transport;

mod client;
mod error;
mod pollable;

pub use client::*;

use pollable::PollableSet;
use proto::*;

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
#[cfg_attr(feature = "ws_transport", derive(Serialize))]
pub struct Uri {
    #[cfg_attr(feature = "ws_transport", serde(flatten))]
    encoded: String,
}
impl Uri {
    /// Constructs a URI from a textual representation, skipping all validation.
    ///
    /// It is highly recommended to use [`relaxed`] or [`strict`] instead, unless you are writing
    /// a transport implementation.
    pub fn raw(text: String) -> Self {
        Uri { encoded: text }
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
            Some(Uri { encoded: text })
        } else {
            None
        }
    }

    /// Constructs and strictly validates a URI from a textual representation.
    ///
    /// Returns `None` if validation fails. A strict validation enforces that URI components only
    /// contain lower-case letters, digits, and "_". Returns `None` if validation fails.
    pub fn strict(text: String) -> Option<Self> {
        lazy_static! {
            // regex taken from WAMP specification
            static ref RE: Regex = Regex::new(r"^(([0-9a-z_]+\.)|\.)*([0-9a-z_]+)?$").unwrap();
        }
        if RE.is_match(&text) && !text.starts_with("wamp.") {
            Some(Uri { encoded: text })
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
            value: thread_rng().gen_range(0, 0x20_0000_0000_0001),
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
#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
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

    /// Asynchronously constructs a transport to the router at the given location.
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
    fn connect(url: &str, handle: &reactor::Handle) -> Result<ConnectResult<Self>, Error>;

    /// Spawns a long-running task which will listen for events and forward them to the
    /// [`ReceivedValues`] returned by the [`connect`] method. Multiple calls to this method
    /// have no effect beyond the first.
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
    fn listen(&mut self, handle: &reactor::Handle);
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
        assert_eq!("foo", Uri::raw("foo".into()).encoded);
        assert_eq!("~~~~1234_*()", Uri::raw("~~~~1234_*()".into()).encoded);
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
        unimplemented!();
    }

    #[test]
    fn id_router_test() {
        unimplemented!();
    }

    #[test]
    fn id_scope_test() {
        unimplemented!();
    }

    // TODO ensure that JSON serialization works as expected
}