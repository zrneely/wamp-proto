
use std::collections::HashMap;
use std::time::Duration;

use failure::Error;
use futures::{future, Future};
use tokio_timer;

use {Id, GlobalScope, RouterScope, Uri, TransportableValue as TV, WampTransport};
use error::WampError;
use proto::{TxMessage, RxMessage};

lazy_static! {
    // Just accept the defaults; performance could degrade very slightly if any timeout is set to
    // more than 409.6 seconds.
    static ref TIMER: tokio_timer::Timer = tokio_timer::wheel().build();
}

/// A factory for WAMP clients.
///
/// This is a typestate for a client which has not yet initialized its connection to a router.
pub struct ClientBuilder<T: WampTransport> {
    transport: T,
    timeout: Duration,
}
impl<T: WampTransport> ClientBuilder<T> {
    /// Creates a client builder which will connect to the given URL using a provider of the
    /// specified type.
    pub fn open_transport(url: &str) -> impl Future<Item=Self> {
        let transport_future = T::connect(url);
        transport_future.map(|transport| ClientBuilder {
            transport,
            timeout: Duration::from_secs(30),
        })
    }

    /// Changes the client's timeout value for protocol-level IO. The default value is 30 seconds.
    ///
    /// This does not affect timeouts for remote procedure calls.
    pub fn set_protocol_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Initializes the WAMP protocol using the given transport, connecting to the provided Realm.
    pub fn initialize(self, realm: Uri) -> impl Future<Item=Client<T>, Error=Error> {
        // Copy to avoid partial move issues
        let timeout1 = self.timeout;
        let timeout2 = self.timeout;

        // Start by sending TxMessage::Hello, ...
        self.transport.send(TxMessage::Hello {
            realm,
            details: {
                let mut details = HashMap::new();
                details.insert("roles".into(), {
                    let mut roles = HashMap::new();
                    if cfg!(feature = "caller") {
                        roles.insert("caller".into(), TV::Dict(Default::default()));
                    }
                    if cfg!(feature = "callee") {
                        roles.insert("callee".into(), TV::Dict(Default::default()));
                    }
                    if cfg!(feature = "subscriber") {
                        roles.insert("subscriber".into(), TV::Dict(Default::default()));
                    }
                    if cfg!(feature = "publisher") {
                        roles.insert("publisher".into(), TV::Dict(Default::default()));
                    }
                    TV::Dict(roles)
                });
                details
            },
        }).and_then(|transport|
            transport.into_future().map_err(|(error, _transport)| error)
        ).and_then(move |(item, transport)| match item { // move to take the cloned timeout
            Some(RxMessage::Welcome { session, details }) => future::ok(Client {
                transport, session,
                timeout: timeout1,
                router_capabilities: RouterCapabilities::from_details(&details)
            }),
            Some(message) => future::err(Error::from(WampError::UnexpectedMessage {
                message,
                expecting: "Welcome"
            })),
            _ => future::err(Error::from(WampError::TransportStreamClosed))
        }).select2(
            TIMER.sleep(timeout2)
        ).then(move |res| match res {
            Ok(future::Either::A((client, _timeout))) => Ok(client),
            Ok(future::Either::B(((), _client))) => Err(WampError::Timeout {
                time: timeout2,
            }.into()),
            Err(future::Either::A((err, _timeout))) => Err(err),
            Err(future::Either::B((_err, _timeout))) => panic!("timeout future failed"),
        })
    }
}

/// A WAMP client.
///
/// By default, this supports all 4 roles supported by this crate. They can be
/// selectively disabled if not necessary (to improve compilation time) with the cargo features
/// `callee`, `caller`, `publisher`, and `subscriber`.
///
/// Create a `Client` with a `ClientBuilder`.
pub struct Client<T: WampTransport> {
    transport: T,
    session: Id<GlobalScope>,
    timeout: Duration,
    router_capabilities: RouterCapabilities,
}
impl <T: WampTransport> Client<T> {

    //#[cfg(feature = "caller")]
    ///// Calls an RPC which has been registered to the router by any client in the same realm as
    ///// this one.
    //fn call(&mut self, name: Uri, args: RpcArgs) -> impl Future<Item=RpcReturn, Error=Error> {
    //    unimplemented!()
    //}

    //#[cfg(feature = "callee")]
    ///// Registers a method with the router. The returned future will complete as soon as
    ///// registration is finished and the method is accessible to other peers.
    //fn register<U: Future<Item=RpcReturn>, F: FnMut(RpcArgs) -> U>(
    //    &mut self,
    //    name: Uri,
    //    function: F
    //) -> impl Future<Item=Registration, Error=Error> {
    //    unimplemented!()
    //}

    //#[cfg(feature = "callee")]
    ///// Unregisters a method with the router. The returned future will complete as soon as
    ///// unregistration is finished and the registered method is inaccessible to other peers.
    //fn unregister(registration: Registration) -> impl Future<Item=(), Error=Error> {
    //    unimplemented!()
    //}

    //#[cfg(feature = "publisher")]
    ///// Publishes a message. The returned future will complete as soon as the protocol-level work
    ///// of sending the message is complete.
    //fn publish(&mut self, topic: Uri, broadcast: Broadcast) -> impl Future<Item=(), Error=Error> {
    //    unimplemented!()
    //}

    //#[cfg(feature = "publisher")]
    ///// Publishes a message, requesting acknowledgement. The returned future will not complete
    ///// until acknowledgement is received. The returned value is the Publication ID of the
    ///// published message.
    //fn publish_with_ack(
    //    &mut self,
    //    topic: Uri,
    //    broadcast: Broadcast
    //) -> impl Future<Item=Id<GlobalScope>, Error=Error> {
    //    unimplemented!()
    //}

    //#[cfg(feature = "subscriber")]
    ///// Subscribes to a channel.
    //fn subscribe<F: FnMut(Broadcast)>(
    //    &mut self,
    //    topic: Uri,
    //    handler: F
    //) -> impl Future<Item=Subscription, Error=Error> {
    //    unimplemented!()
    //}

    //#[cfg(feature = "subscriber")]
    ///// Unsubscribes from a channel.
    //fn unsubscribe(&mut self, subscription: Subscription) -> impl Future<Item=(), Error=Error> {
    //    unimplemented!()
    //}
}

struct RouterCapabilities {
    broker: bool,
    dealer: bool,
    // any advanced profile features should go here as extra bools or by converting an above bool
    // to some other type
}
impl RouterCapabilities {
    fn from_details(details: &HashMap<String, TV>) -> Self {
        match details.get("roles") {
            Some(&TV::Dict(ref roles)) => RouterCapabilities {
                broker: roles.contains_key("broker"),
                dealer: roles.contains_key("dealer"),
            },
            _ => RouterCapabilities {
                broker: false,
                dealer: false,
            }
        }
    }
}

#[cfg(any(feature = "caller", feature = "callee"))]
/// Arguments to an RPC call.
pub struct RpcArgs {
    /// The positional arguments. If there are none, the `Vec` will be empty.
    pub list: Vec<TV>,
    /// The keyword arguments. If there are none, the `HashMap` will be empty.
    pub dict: HashMap<String, TV>,
}

#[cfg(any(feature = "caller", feature = "callee"))]
/// Return value from an RPC call.
pub struct RpcReturn {
    /// Positional return values. If there are none, set this to an empty `Vec`.
    pub list: Vec<TV>,
    /// Keyword return values. If there are none, set this to an empty `HashMap`.
    pub dict: HashMap<String, TV>,
}

#[cfg(feature = "callee")]
/// The result of registering a function as an RPC.
#[derive(Debug)]
pub struct Registration {
    // The ID of the registration, chosen by the router.
    id: Id<RouterScope>,
}

#[cfg(any(feautre = "publisher", feature = "subscriber"))]
/// A message broadcast on a channel.
pub struct Broadcast {
    /// Positional values. If there are none, this should be an empty `Vec`.
    pub list: Vec<TV>,
    /// Keyword values. If there are none, this should be an empty `HashMap`.
    pub dict: HashMap<String, TV>,
}

#[cfg(feature = "subscriber")]
/// The result of subscribing to a channel. Can be used to unsubscribe.
#[derive(Debug)]
pub struct Subscription {
    // The ID of the subscription, chosen by the router.
    id: Id<RouterScope>,
}
