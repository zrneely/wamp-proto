
//! A simple, futures-based WAMP client implementation, capable of taking the roles caller,
//! callee, publisher, and subscriber.
//!
//! It is extensible to any suitable transport.
//!
//! This crate implements the basic profile, and (for now) *none* of the advanced profile.

#![deny(missing_docs)]
#![feature(conservative_impl_trait)]

extern crate chrono;
extern crate failure;
#[macro_use]
extern crate failure_derive;
extern crate futures;
#[macro_use]
extern crate lazy_static;
extern crate rand;
extern crate regex;
extern crate serde;
extern crate tokio_timer;
#[cfg(feature = "websocket")]
extern crate websocket;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::collections::HashMap;
use std::marker::PhantomData;

use failure::Error;
use futures::prelude::*;
use rand::{thread_rng, Rng};
use regex::Regex;

/// Contains protocol-level details.
///
/// If you aren't defining your own transport type, you shouldn't need to worry about this module.
pub mod proto;

mod client;
mod error;

pub use client::*;

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
/// An example of a well-formed URI is `"org.foobar.application.service"`.
#[derive(Debug)]
pub struct Uri {
    encoded: String,
}
impl Uri {
    /// Constructs and validates a URI from a textual representation.
    ///
    /// Returns `None` if validation fails.
    pub fn relaxed(text: String) -> Option<Self> {
        lazy_static! {
            // regex taken from WAMP specification
            static ref RE: Regex = Regex::new(r"^(([^\s\.#]+\.)|\.)*([^\s\.#]+)$").unwrap();
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

/// IDs in the global scope are chosen randomly from a uniform distribution over the possible
/// values of an ID.
#[derive(Debug)]
pub enum GlobalScope {}

/// IDs in the router scope are chosen by the router using an arbitrary algorithm.
#[derive(Debug)]
pub enum RouterScope {}

/// IDs in the session scope are incremented by 1, from 1.
#[derive(Debug)]
pub enum SessionScope {}

/// A WAMP ID is used to identify sessions, publications, subscriptions, registrations, and
/// requests.
///
/// WAMP IDs are internally integers between 0 and 2^53 (inclusive).
///
/// The type parameter should be one of `GlobalScope`, `RouterScope`, or `SessionScope`. It is a
/// compile time-only value which describes the scope of the ID.
#[derive(Debug)]
pub struct Id<S> {
    value: u64,
    _pd: PhantomData<S>,
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
    /// `std::usize::max_value()` IDs. This method is thread-safe.
    pub fn next() -> Self {
        lazy_static! {
            static ref NEXT: AtomicUsize = AtomicUsize::new(1);
        }
        Id {
            value: NEXT.fetch_add(1, Ordering::Relaxed) as u64,
            _pd: PhantomData,
        }
    }
}

/// The types of value which can be sent over WAMP RPC and pub/sub boundaries.
#[derive(Debug)]
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
    fn into_int(self) -> Option<u64> {
        match self {
            TransportableValue::Integer(x) => Some(x),
            _ => None,
        }
    }
    fn into_string(self) -> Option<String> {
        match self {
            TransportableValue::String(x) => Some(x),
            _ => None,
        }
    }
    fn into_bool(self) -> Option<bool> {
        match self {
            TransportableValue::Bool(x) => Some(x),
            _ => None,
        }
    }
    fn into_list(self) -> Option<Vec<TransportableValue>> {
        match self {
            TransportableValue::List(x) => Some(x),
            _ => None,
        }
    }
    fn into_dict(self) -> Option<HashMap<String, TransportableValue>> {
        match self {
            TransportableValue::Dict(x) => Some(x),
            _ => None,
        }
    }
}

/// A transport capable of supporting a WAMP connection.
///
/// While WAMP was designed with Websockets in mind, any message transport can be used if it is
/// message-based, bidirectional, ordered, and reliable. By default, this crate has the
/// "websocket" feature enabled, which allows it to provide 2 websocket-based transports: one which
/// uses JSON for serialization, and one which uses msgpack. Other ransports may be written by
/// implementing this trait.
///
/// A transport is a `Stream` for `proto::RxMessage` objects and a `Sink` for `proto::TxMessage`
/// objects.
pub trait WampTransport: Stream<Item=RxMessage, Error=Error> + Sink<SinkItem=TxMessage, SinkError=Error> {

    /// Asynchronously constructs a transport to the router at the given location.
    ///
    /// The format of the location string is implementation defined, but will probably be a URL.
    /// This method should do asynchronous work such as opening a TCP socket or initializing a
    /// websocket connection.
    fn connect(url: &str) -> Box<Future<Item=Self, Error=Error>> where Self: Sized;
}

#[cfg(test)]
mod tests {

    #[test]
    fn uri_relaxed() {
        // TODO test positive and negative test cases
        unimplemented!();
    }

    #[test]
    fn uri_strict() {
        // TODO test positive and negative test cases
        unimplemented!();
    }
}
