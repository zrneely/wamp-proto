//! A futures-based WAMP client implementation, capable of taking the roles caller, callee,
//! publisher, and subscriber.
//!
//! It is extensible to any suitable transport. Transport implementation are provided via
//! other crates.
//!
//! This crate implements the WAMP basic profile, and (for now) *none* of the advanced profile.

#![deny(missing_docs)]
#![allow(dead_code)]
#![recursion_limit = "512"] // for a big select! in client/ops/initialize.rs

#[macro_use]
extern crate failure_derive;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate log;
#[macro_use]
extern crate serde_derive;

#[cfg(feature = "ws_transport")]
#[macro_use]
extern crate serde_json;

use std::fmt;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};

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
        Ok(u64::from(value))
    }

    fn visit_u16<E: de::Error>(self, value: u16) -> Result<u64, E> {
        Ok(u64::from(value))
    }

    fn visit_u32<E: de::Error>(self, value: u32) -> Result<u64, E> {
        Ok(u64::from(value))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<u64, E> {
        if value > MAX_ID_VAL {
            Err(E::custom(format!("u64 out of range: {}", value)))
        } else {
            Ok(value)
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

/// A strucutred map of incoming ERROR messages.
pub struct Errors {
    #[cfg(feature = "subscriber")]
    pub subscribe: PollableSet<rx::Error>,
    #[cfg(feature = "subscriber")]
    pub unsubscribe: PollableSet<rx::Error>,

    #[cfg(feature = "publisher")]
    pub publish: PollableSet<rx::Error>,

    #[cfg(feature = "callee")]
    pub register: PollableSet<rx::Error>,
    #[cfg(feature = "callee")]
    pub unregister: PollableSet<rx::Error>,

    #[cfg(feature = "caller")]
    pub call: PollableSet<rx::Error>,
}

/// Acts as a buffer for each type of returned message, and any possible errors
/// (either in the transport layer or the protocol).
#[derive(Default, Debug)]
pub struct MessageBuffer {
    /// The buffer of incoming "GOODBYE" messages. This is not populated with
    /// messages unless the client triggered the disconned.
    pub goodbye: PollableSet<rx::Goodbye>,

    /// The buffer of incoming "ERROR" messages.
    pub errors: Errors,

    /// The buffer of incoming "SUBSCRIBED" messages.
    #[cfg(feature = "subscriber")]
    pub subscribed: PollableSet<rx::Subscribed>,
    /// The buffer of incoming "UNSUBSCRIBED" messages.
    #[cfg(feature = "subscriber")]
    pub unsubscribed: PollableSet<rx::Unsubscribed>,
    /// The buffer of incoming "EVENT" messages.
    #[cfg(feature = "subscriber")]
    pub event: PollableSet<rx::Event>,

    /// The buffer of incoming "PUBLISHED" messages.
    #[cfg(feature = "publisher")]
    pub published: PollableSet<rx::Published>,

    /// The buffer of incoming "REGISTERED" messages.
    #[cfg(feature = "callee")]
    pub registered: PollableSet<rx::Registered>,
    /// The buffer of incoming "UNREGISTERED" messages.
    #[cfg(feature = "callee")]
    pub unregistered: PollableSet<rx::Unregistered>,
    /// The buffer of incoming "INVOCATION" messages.
    #[cfg(feature = "callee")]
    pub invocation: PollableSet<rx::Invocation>,

    /// The buffer of incoming "RESULT" messages.
    #[cfg(feature = "caller")]
    pub result: PollableSet<rx::Result>,
}
impl MessageBuffer {
    #[cfg(test)]
    fn len(&self) -> usize {
        let mut len = self.welcome.len() + self.abort.len() + self.goodbye.len() + self.error.len();

        #[cfg(feature = "subscriber")]
        {
            len += self.subscribed.len() + self.unsubscribed.len() + self.event.len();
        }

        #[cfg(feature = "publisher")]
        {
            len += self.published.len();
        }

        #[cfg(feature = "callee")]
        {
            len += self.registered.len() + self.unregistered.len() + self.invocation.len();
        }

        #[cfg(feature = "caller")]
        {
            len += self.result.len();
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
