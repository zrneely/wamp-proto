
use std::collections::HashMap;

use {GlobalScope, Id, RouterScope, SessionScope, TransportableValue, Uri};

/// Raw message codes for each message type.
#[allow(missing_docs)]
pub mod msg_code {
    pub const HELLO: u8 = 1;
    pub const WELCOME: u8 = 2;
    pub const ABORT: u8 = 3;
    pub const GOODBYE: u8 = 6;
    pub const ERROR: u8 = 8;
    pub const PUBLISH: u8 = 16;
    pub const PUBLISHED: u8 = 17;
    pub const SUBSCRIBE: u8 = 32;
    pub const SUBSCRIBED: u8 = 33;
    pub const UNSUBSCRIBE: u8 = 34;
    pub const UNSUBSCRIBED: u8 = 35;
    pub const EVENT: u8 = 36;
    pub const CALL: u8 = 48;
    pub const RESULT: u8 = 50;
    pub const REGISTER: u8 = 64;
    pub const REGISTERED: u8 = 65;
    pub const UNREGISTER: u8 = 66;
    pub const UNREGISTERED: u8 = 67;
    pub const INVOCATION: u8 = 68;
    pub const YIELD: u8 = 70;
}

type Dict = HashMap<String, TransportableValue>;
type List = Vec<TransportableValue>;

/// The various types of messages which can be sent by the client as part of WAMP. For more
/// details, see [the WAMP protocol specification].
///
/// [the WAMP protocol specification]: http://wamp-proto.org/spec/
#[derive(Debug)]
#[allow(missing_docs)]
pub enum TxMessage {
    Hello {
        realm: Uri,
        details: Dict,
    },
    Goodbye {
        details: Dict,
        reason: Uri,
    },
    Error {
        // TODO
    },
    Publish {
        // TODO
    },
    Subscribe {
        request: Id<SessionScope>,
        options: Dict,
        topic: Uri,
    },
    Unsubscribe {
        // TODO
    },
    Call {
        // TODO
    },
    Register {
        // TODO
    },
    Unregister {
        // TODO
    },
    Yield {
        // TODO
    },
}

/// The various types of messages which can be received by the client as part of WAMP. For more
/// details, see [the WAMP protocol specification].
///
/// [the WAMP protocol specification]: http://wamp-proto.org/spec/
#[allow(missing_docs)]
pub mod rx {
    use super::*;

    macro_rules! rx_message_type {
        ($name:ident [ $code_name:ident ] { $($fields:tt)* }) => {
            #[derive(Debug, Eq, PartialEq)]
            pub struct $name {
                $($fields)*
            }
            impl RxMessage for $name {
                const MSG_CODE: u64 = msg_code::$code_name as u64;
            }
        };
    }

    /// Marker trait for received messages. Do not implement this yourself.
    pub trait RxMessage {
        /// The identifying integer for this message.
        const MSG_CODE: u64;
    }

    // Session management; used by all types of peers.
    rx_message_type!(Welcome [WELCOME] {
        pub session: Id<GlobalScope>,
        pub details: Dict,
    });

    // Session management; used by all types of peers.
    rx_message_type!(Abort [ABORT] {
        pub details: Dict,
        pub reason: Uri,
    });

    // Session management; used by all types of peers.
    rx_message_type!(Goodbye [GOODBYE] {
        pub details: Dict,
        pub reason: Uri,
    });

    // Message type used by all roles to indicate problems with a request.
    rx_message_type!(Error [ERROR] {
        pub request_type: u64,
        pub request: Id<SessionScope>,
        pub details: Dict,
        pub error: Uri,
        pub arguments: Option<List>,
        pub arguments_kw: Option<Dict>,
    });

    // Sent by brokers to subscribers after they are subscribed to a topic.
    #[cfg(feature = "subscriber")]
    rx_message_type!(Subscribed [SUBSCRIBED] {
        pub request: Id<SessionScope>,
        pub subscription: Id<RouterScope>,
    });

    // Sent by brokers to subscribers after they are unsubscribed from a topic.
    #[cfg(feature = "subscriber")]
    rx_message_type!(Unsubscribed [UNSUBSCRIBED] {
        pub request: Id<SessionScope>,
    });

    // Sent by borkers to subscribers to indicate that a message was published to a topic.
    #[cfg(feature = "subscriber")]
    rx_message_type!(Event [EVENT] {
        pub subscription: Id<RouterScope>,
        pub publication: Id<RouterScope>,
        pub details: Dict,
        pub arguments: Option<List>,
        pub arguments_kw: Option<Dict>,
    });

    // Sent by brokers to publishers after they publish a message to a topic, if they
    // requested acknowledgement.
    #[cfg(feature = "publisher")]
    rx_message_type!(Published [PUBLISHED] {
        pub request: Id<SessionScope>,
        pub publication: Id<RouterScope>,
    });

    // Sent by dealers to callees after an RPC is registered.
    #[cfg(feature = "callee")]
    rx_message_type!(Registered [REGISTERED] {
        pub request: Id<SessionScope>,
        pub registration: Id<RouterScope>,
    });

    // Sent by dealers to callees after an RPC is unregistered.
    #[cfg(feature = "callee")]
    rx_message_type!(Unregistered [UNREGISTERED] {
        pub request: Id<SessionScope>,
    });

    // Sent by dealers to callees when an RPC they have registered is invoked.
    #[cfg(feature = "callee")]
    rx_message_type!(Invocation [INVOCATION] {
        pub request: Id<SessionScope>,
        pub registration: Id<RouterScope>,
        pub details: Dict,
        pub arguments: Option<List>,
        pub arguments_kw: Option<Dict>,
    });

    // Sent by dealers to callers when an RPC they invoked has completed.
    #[cfg(feature = "caller")]
    rx_message_type!(Result [RESULT] {
        pub request: Id<SessionScope>,
        pub details: Dict,
        pub arguments: Option<List>,
        pub arguments_kw: Option<Dict>,
    });
}
