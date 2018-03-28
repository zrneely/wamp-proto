
use std::collections::HashMap;

use {GlobalScope, Id, RouterScope, TransportableValue, Uri};

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
    Abort {
        details: Dict,
        reason: Uri,
    },
    Goodbye {
        details: Dict,
        reason: Uri,
    },
    Subscribe {
        request: Id<GlobalScope>,
        options: Dict,
        topic: Uri,
    },
}

/// The various types of messages which can be received by the client as part of WAMP. For more
/// details, see [the WAMP protocol specification].
///
/// [the WAMP protocol specification]: http://wamp-proto.org/spec/
#[allow(missing_docs)]
pub mod rx {
    use super::*;

    /// Marker trait for received messages. Do not implement this yourself.
    pub trait Message {
        /// The identifying integer for this message.
        const MSG_CODE: u8;
    }

    #[derive(Debug, Eq, PartialEq)]
    pub struct Welcome {
        pub session: Id<GlobalScope>,
        pub details: Dict,
    }
    impl Message for Welcome {
        const MSG_CODE: u8 = msg_code::WELCOME;
    }

    #[derive(Debug, Eq, PartialEq)]
    pub struct Abort {
        pub details: Dict,
        pub reason: Uri,
    }
    impl Message for Abort {
        const MSG_CODE: u8 = msg_code::ABORT;
    }

    #[derive(Debug, Eq, PartialEq)]
    pub struct Goodbye {
        pub details: Dict,
        pub reason: Uri,
    }
    impl Message for Goodbye {
        const MSG_CODE: u8 = msg_code::GOODBYE;
    }

    #[derive(Debug, Eq, PartialEq)]
    pub struct Subscribed {
        pub request: Id<GlobalScope>,
        pub subscription: Id<RouterScope>,
    }
    impl Message for Subscribed {
        const MSG_CODE: u8 = msg_code::SUBSCRIBED;
    }
}
