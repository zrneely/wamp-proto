
use std::collections::HashMap;

use {GlobalScope, Id, TransportableValue, Uri};

mod msg_code {
    const MSG_CODE_HELLO: u64 = 1;
    const MSG_CODE_WELCOME: u64 = 2;
    const MSG_CODE_ABORT: u64 = 3;
    const MSG_CODE_GOODBYE: u64 = 6;
    const MSG_CODE_ERROR: u64 = 8;
}

// TODO the rest of the message types in the basic profile

type Dict = HashMap<String, TransportableValue>;
type List = Vec<TransportableValue>;

/// The various types of messages which can be sent by the client as part of WAMP.
#[derive(Debug)]
#[allow(missing_docs)] // TODO remove
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
}

/// The various types of messages which can be received by the client as part of WAMP.
#[derive(Debug)]
#[allow(missing_docs)] // TODO remove
pub enum RxMessage {
    Welcome {
        session: Id<GlobalScope>,
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
}
