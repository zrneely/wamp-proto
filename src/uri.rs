use std::fmt;

use regex::Regex;

/// The URIs known to WAMP.
pub mod known_uri {
    #![allow(non_upper_case_globals)]

    macro_rules! w_uri {
        ($doc:expr, $name:ident) => {
            #[doc=$doc]
            pub const $name: &'static str = concat!("wamp.error.", stringify!($name));
        };
    }

    // Defined in section 5.3.3
    w_uri!(
        "Used when a peer commits a protocol violation.",
        protocol_violation
    );

    /// URIs defined in section 11.1.2, which deal with receiving invalid URIs.
    pub mod interaction {
        w_uri!("Used when a peer provides an invalid URI.", invalid_uri);
        w_uri!(
            "Used when an RPC is invoked which doesn't exist.",
            no_such_procedure
        );
        w_uri!(
            "Used when a callee tries to register an RPC which is already registered.",
            procedure_already_exists
        );
        w_uri!(
            "Used when a callee tries to unregister an RPC which isn't registered.",
            no_such_registration
        );
        w_uri!(
            "Used when a subscriber tries to unsubscribe from a topic that they aren't
             subscribed to.",
            no_such_subscription
        );
        w_uri!(
            "A caller invoked an RPC with arguments that are invalid.",
            invalid_argument
        );
    }

    /// URIs defined in section 11.1.3, which deal with closing sessions.
    pub mod session_close {
        w_uri!(
            "The connected peer is shutting down entirely.",
            system_shutdown
        );
        w_uri!(
            "The connected peer wants to leave the realm it's connected to.",
            close_realm
        );
        w_uri!(
            "The connected peer is acknowledging a GOODBYE message.",
            goodbye_and_out
        );
        w_uri!(
            "The connected peer received an invalid protocol message.",
            protocol_violation
        );
    }

    /// URIs defined in section 11.1.4, which deal with authorization.
    pub mod authorization {
        w_uri!(
            "Some action failed because the peer is not authorized.",
            not_authorized
        );
        w_uri!(
            "An attempt to determine if the peer is authorized failed.",
            authorization_failed
        );
        w_uri!(
            "The connected peer wanted to join a non-existant realm.",
            no_such_realm
        );
        w_uri!(
            "A peer tried to authenticate with a role that does not exist.",
            no_such_role
        );
    }
}

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
    pub fn relaxed<T: AsRef<str>>(text: T) -> Option<Self> {
        lazy_static! {
            // regex taken from WAMP specification
            static ref RE: Regex = Regex::new(r"^([^\s\.#]+\.)*([^\s\.#]+)$").unwrap();
        }

        if RE.is_match(&text.as_ref()) {
            Some(Uri(text.as_ref().to_string()))
        } else {
            None
        }
    }

    /// Constructs and strictly validates a URI from a textual representation.
    ///
    /// Returns `None` if validation fails. A strict validation enforces that URI components only
    /// contain lower-case letters, digits, and "_".
    pub fn strict<T: AsRef<str>>(text: T) -> Option<Self> {
        lazy_static! {
            // regex taken from WAMP specification
            static ref RE: Regex = Regex::new(r"^(([0-9a-z_]+\.)|\.)*([0-9a-z_]+)?$").unwrap();
        }

        if RE.is_match(&text.as_ref()) {
            Some(Uri(text.as_ref().to_string()))
        } else {
            None
        }
    }

    /// Copies the raw string representation of this Uri and returns it.
    pub fn to_raw(&self) -> String {
        self.0.clone()
    }
}
impl fmt::Display for Uri {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl<'a> PartialEq<&'a str> for Uri {
    fn eq(&self, other: &&'a str) -> bool {
        let Uri(ref val) = self;
        val == other
    }
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
            "wamp.f",
        ];
        let negative_tests = ["a.#.c.d", "..", "a..b.c.d", "a. .b.c.d", "a .b.c.d"];

        for &test in positive_tests.iter() {
            println!("asserting that {} is a valid relaxed URI", test);
            assert!(Uri::relaxed(test).is_some());
        }

        for &test in negative_tests.iter() {
            println!("asserting that {} is an invalid relaxed URI", test);
            assert!(Uri::relaxed(test).is_none());
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
            "wamp.f",
        ];
        let negative_tests = [
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
            assert!(Uri::strict(test).is_some());
        }

        for &test in negative_tests.iter() {
            println!("asserting that {} is an invalid strict URI", test);
            assert!(Uri::strict(test).is_none());
        }
    }
}
