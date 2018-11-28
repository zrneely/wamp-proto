use std::collections::HashMap;

use {GlobalScope, Id, RouterScope, SessionScope, TransportableValue, Uri};

/// Raw message codes for each message type.
#[allow(missing_docs)]
mod msg_code {
    pub const HELLO: u64 = 1;
    pub const WELCOME: u64 = 2;
    pub const ABORT: u64 = 3;
    pub const GOODBYE: u64 = 6;
    pub const ERROR: u64 = 8;
    pub const PUBLISH: u64 = 16;
    pub const PUBLISHED: u64 = 17;
    pub const SUBSCRIBE: u64 = 32;
    pub const SUBSCRIBED: u64 = 33;
    pub const UNSUBSCRIBE: u64 = 34;
    pub const UNSUBSCRIBED: u64 = 35;
    pub const EVENT: u64 = 36;
    pub const CALL: u64 = 48;
    pub const RESULT: u64 = 50;
    pub const REGISTER: u64 = 64;
    pub const REGISTERED: u64 = 65;
    pub const UNREGISTER: u64 = 66;
    pub const UNREGISTERED: u64 = 67;
    pub const INVOCATION: u64 = 68;
    pub const YIELD: u64 = 70;
}

type Dict = HashMap<String, TransportableValue>;
type List = Vec<TransportableValue>;

/// The various types of messages which can be sent by the client as part of WAMP. For more
/// details, see [the WAMP protocol specification].
///
/// [the WAMP protocol specification]: http://wamp-proto.org/spec/
#[derive(Debug)]
pub enum TxMessage {
    /// Session management; used by all types of peers.
    Hello {
        /// The realm to connect to.
        realm: Uri,
        /// Describes this peer's capabilities. Could also include a user-agent string.
        details: Dict,
    },

    /// Session management; used by all types of peers.
    Goodbye {
        /// Allows providing optional, additional information.
        details: Dict,
        /// A somewhat known URI describing why the session is being closed.
        reason: Uri,
    },

    /// Message type used by all roles to indicate a problem with a request.
    Error {
        /// The message code for the type of message that caused the error.
        request_type: u64,
        /// The ID of the request that caused the error.
        request: Id<SessionScope>,
        /// Optional details describing the error.
        details: Dict,
        /// A somewhat known URI describing the error.
        error: Uri,
        /// A list of positional data.
        arguments: Option<List>,
        /// A dictionary of key-value data.
        arguments_kw: Option<Dict>,
    },

    /// Sent by publishers to brokers when they have a message to send.
    #[cfg(feature = "publisher")]
    Publish {
        /// A request ID.
        request: Id<SessionScope>,
        /// Optional additional parameters for the publication.
        options: Dict,
        /// The topic to publish to.
        topic: Uri,
        /// An optional list of positional data.
        arguments: Option<List>,
        /// An optional dictionary of key-value data.
        arguments_kw: Option<Dict>,
    },

    /// Sent by subscribers to brokers when they wish to receive publications sent to a
    /// particular topic.
    #[cfg(feature = "subscriber")]
    Subscribe {
        /// A request ID.
        request: Id<SessionScope>,
        /// Optional additional parameters for the requested subscription.
        options: Dict,
        /// The topic to subscribe to.
        topic: Uri,
    },

    /// Sent by subscribers to brokers when they no longer wish to receive publications sent
    /// to a particular topic.
    #[cfg(feature = "subscriber")]
    Unsubscribe {
        /// A request ID.
        request: Id<SessionScope>,
        /// The ID of the subscription to cancel.
        subscription: Id<RouterScope>,
    },

    /// Sent by callers to dealers when they wish to invoke an RPC.
    #[cfg(feature = "caller")]
    Call {
        /// A request ID.
        request: Id<SessionScope>,
        /// Optional additional parameters for the call.
        options: Dict,
        /// The procedure to invoke.
        procedure: Uri,
        /// The positional arguments to the procedure.
        arguments: Option<List>,
        /// The key-value arguments to the procedure.
        arguments_kw: Option<Dict>,
    },

    /// Sent by callees to dealers to register a new RPC.
    #[cfg(feature = "callee")]
    Register {
        /// A request ID.
        request: Id<SessionScope>,
        /// Optional additional parameters for the registration.
        options: Dict,
        /// The procedure to register.
        procedure: Uri,
    },

    /// Sent by callees to dealers to remove an existing RPC registration.
    #[cfg(feature = "callee")]
    Unregister {
        /// A request ID.
        request: Id<SessionScope>,
        /// The ID of the registration to remove.
        registration: Id<RouterScope>,
    },

    /// Sent by callees to dealers to indicate that an RPC is finished.
    #[cfg(feature = "callee")]
    Yield {
        /// The request ID from the invocation.
        request: Id<RouterScope>,
        /// Optional additional parameters for the yielded data.
        options: Dict,
        /// Positional returned data.
        arguments: Option<List>,
        /// Key-value returned data.
        arguments_kw: Option<Dict>,
    },
}
impl TxMessage {
    /// Determines the message code for this message.
    // TODO: UT for this (ugh)
    pub fn get_message_code(&self) -> u64 {
        use TxMessage::*;

        match self {
            Hello { .. } => msg_code::HELLO,
            Goodbye { .. } => msg_code::GOODBYE,
            Error { .. } => msg_code::ERROR,

            #[cfg(feature = "publisher")]
            Publish { .. } => msg_code::PUBLISH,

            #[cfg(feature = "subscriber")]
            Subscribe { .. } => msg_code::SUBSCRIBE,

            #[cfg(feature = "subscriber")]
            Unsubscribe { .. } => msg_code::UNSUBSCRIBE,

            #[cfg(feature = "caller")]
            Call { .. } => msg_code::CALL,

            #[cfg(feature = "callee")]
            Register { .. } => msg_code::REGISTER,

            #[cfg(feature = "callee")]
            Unregister { .. } => msg_code::UNREGISTER,

            #[cfg(feature = "callee")]
            Yield { .. } => msg_code::YIELD,
        }
    }

    /// Converts this message to a JSON representation.
    // TODO: UT for this (ugh)
    #[cfg(feature = "serde_json")]
    pub fn to_json(&self) -> serde_json::Value {
        use TxMessage::*;

        let code = self.get_message_code();
        match self {
            Hello {
                ref realm,
                ref details,
            } => json!([code, realm, details]),

            Goodbye {
                ref details,
                ref reason,
            } => json!([code, details, reason]),

            Error {
                ref request_type,
                ref request,
                ref details,
                ref error,
                ref arguments,
                ref arguments_kw,
            } => {
                if let Some(ref arguments) = arguments {
                    if let Some(ref arguments_kw) = arguments_kw {
                        json!([
                            code,
                            request_type,
                            request,
                            details,
                            error,
                            arguments,
                            arguments_kw,
                        ])
                    } else {
                        json!([code, request_type, request, details, error, arguments])
                    }
                } else {
                    json!([code, request_type, request, details, error])
                }
            }

            #[cfg(feature = "publisher")]
            Publish {
                ref request,
                ref options,
                ref topic,
                ref arguments,
                ref arguments_kw,
            } => {
                if let Some(ref arguments) = arguments {
                    if let Some(ref arguments_kw) = arguments_kw {
                        json!([code, request, options, topic, arguments, arguments_kw])
                    } else {
                        json!([code, request, options, topic, arguments])
                    }
                } else {
                    json!([code, request, options, topic])
                }
            }

            #[cfg(feature = "subscriber")]
            Subscribe {
                ref request,
                ref options,
                ref topic,
            } => json!([code, request, options, topic]),

            #[cfg(feature = "subscriber")]
            Unsubscribe {
                ref request,
                ref subscription,
            } => json!([code, request, subscription]),

            #[cfg(feature = "caller")]
            Call {
                ref request,
                ref options,
                ref procedure,
                ref arguments,
                ref arguments_kw,
            } => {
                if let Some(ref arguments) = arguments {
                    if let Some(ref arguments_kw) = arguments_kw {
                        json!([code, request, options, procedure, arguments, arguments_kw])
                    } else {
                        json!([code, request, options, procedure, arguments])
                    }
                } else {
                    json!([code, request, options, procedure])
                }
            }

            #[cfg(feature = "callee")]
            Register {
                ref request,
                ref options,
                ref procedure,
            } => json!([code, request, options, procedure]),

            #[cfg(feature = "callee")]
            Unregister {
                ref request,
                ref registration,
            } => json!([code, request, registration]),

            #[cfg(feature = "callee")]
            Yield {
                ref request,
                ref options,
                ref arguments,
                ref arguments_kw,
            } => {
                if let Some(ref arguments) = arguments {
                    if let Some(ref arguments_kw) = arguments_kw {
                        json!([code, request, options, arguments, arguments_kw])
                    } else {
                        json!([code, request, options, arguments])
                    }
                } else {
                    json!([code, request, options])
                }
            }
        }
    }
}

/// The various types of messages which can be received by the client as part of WAMP. For more
/// details, see [the WAMP protocol specification].
///
/// [the WAMP protocol specification]: http://wamp-proto.org/spec/
#[allow(missing_docs)]
// TODO: convert to enum
pub mod rx {
    use super::*;

    macro_rules! rx_message_type {
        ($name:ident [ $code_name:ident ] { $($fields:tt)* }) => {
            #[derive(Debug, Eq, PartialEq)]
            pub struct $name {
                $($fields)*
            }
            impl RxMessage for $name {
                const MSG_CODE: u64 = msg_code::$code_name;
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
        pub publication: Id<GlobalScope>,
        pub details: Dict,
        pub arguments: Option<List>,
        pub arguments_kw: Option<Dict>,
    });

    // Sent by brokers to publishers after they publish a message to a topic, if they
    // requested acknowledgement.
    #[cfg(feature = "publisher")]
    rx_message_type!(Published [PUBLISHED] {
        pub request: Id<SessionScope>,
        pub publication: Id<GlobalScope>,
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
