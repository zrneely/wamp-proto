
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use failure::Error;
use futures::{Async, future::{self, Either, Future}};
use parking_lot::{Mutex, RwLock};
use tokio::timer::Delay;

use {
    ConnectResult, Id, GlobalScope, ReceivedValues, RouterScope, Uri, TransportableValue as TV, Transport,
    known_uri
};
use error::WampError;

mod initialize;
mod close;

#[cfg(feature = "subscriber")]
mod subscribe;

#[cfg(feature = "subscriber")]
pub use self::subscribe::Subscription;

fn check_for_timeout_or_error(
    timeout: &mut Delay,
    received_vals: &mut ReceivedValues
) -> Result<(), Error> {
    match timeout.poll()? {
        Async::Ready(_) => return Err(WampError::Timeout.into()),
        _ => {}
    };

    // TODO: check for all errors specified by WAMP

    Ok(())
}

struct BroadcastHandler {
    target: Box<Fn(Broadcast) -> Box<Future<Item = (), Error = Error>> + Send>,
}

// The states a client can be in, according to the WAMP specification.
// Clients are not made available to consumers of this library until they reach
// their initial "Established" state.
#[derive(Debug, Eq, PartialEq)]
enum ClientState {
    // The connection to the service is closed.
    Closed,
    // The client has sent the initial "HELLO" message, but not received a response.
    Establishing,
    // The client received anything other than a "WELCOME" while waiting for a "WELCOME",
    // or received "HELLO" or "AUTHENTICATE" while "Established".
    Failed,
    // The client is authenticating the router or the router is authenticating the client.
    Authenticating,
    // The connection is established and PUB/SUB and/or RPC messages can be exchanged with
    // the router freely.
    Established,
    // The client initiated a clean connection termination, and the router has not yet responded.
    ShuttingDown,
    // The router initiated a clean connection termination, and the client has not yet responded.
    Closing,
}

/// Configuration options for a new client.
pub struct ClientConfig<'a> {
    /// The URL to connect to.
    pub url: &'a str,
    /// The realm to connect to.
    pub realm: Uri,
    /// The timeout for basic requests to the router (subscribing, publishing, etc). This
    /// does not effect shutdown or RPC invocations, each of which have their own timeouts.
    pub timeout: Duration,
    /// The maximum time to wait for a "GOODBYE" message from the router in response to
    /// a client-initiated disconnection.
    pub shutdown_timeout: Duration,
}

/// A WAMP client.
///
/// By default, this supports all 4 roles supported by this crate. They can be
/// selectively enabled if not all necessary (to improve compilation time) with the Cargo
/// features `callee`, `caller`, `publisher`, and `subscriber`.
pub struct Client<T: Transport> {
    sender: Arc<Mutex<T>>,
    received: ReceivedValues,

    session_id: Id<GlobalScope>,
    timeout_duration: Duration,
    shutdown_timeout_duration: Duration,
    router_capabilities: RouterCapabilities,

    state: Arc<RwLock<ClientState>>,

    #[cfg(feature = "subscriber")]
    subscriptions: Arc<Mutex<HashMap<Subscription, BroadcastHandler>>>,
}
impl<T: Transport> std::fmt::Debug for Client<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "Client {{\n\treceived: {:?}\n\tsession_id: {:?}\n\ttimeout_duration: {:?}\n\trouter_capabilities: {:?}\n}}",
            self.received, self.session_id, self.timeout_duration, self.router_capabilities)
    }
}
impl <T: Transport> Client<T> {
    /// Begins initialization of a new [`Client`]. See [`ClientConfig`] for details.
    ///
    /// The client will attempt to connect to the WAMP router at the given URL and join the given
    /// realm; the future will not resolve until both of those tasks succeed and the connection
    /// is established.
    ///
    /// This method will handle initialization of the [`Transport`] used, but the caller must
    /// specify the type of transport.
    pub fn new<'a>(config: ClientConfig<'a>) -> impl Future<Item = Self, Error = Error> {
        let ClientConfig { url, realm, timeout, shutdown_timeout } = config;
        let ConnectResult { future, received_values } = T::connect(url);

        future.and_then(move |transport| {
            initialize::InitializeFuture::new(transport, received_values, realm, timeout, shutdown_timeout)
        })
    }

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

    /// Subscribes to a channel.
    ///
    /// The provided broadcast handler must not block! Any potentially blocking operations it must
    /// perform should be handled by returning a [`Future`], which will be spawned as an subtask
    /// on the executor that drives the returned future. If the handler does not need to perform any
    /// blocking operations, it should return a leaf future (likely created by [`future::ok`]). If the
    /// returned future fails, the error will be propogated up to the parent future (the one
    /// returned by this) method.
    ///
    /// # Return Value
    ///
    /// This method returns [`Ok`] if the router supports the "broker" role, and [`Err`] if it
    /// doesn't. The future will return a successful result if subscription was successful, and an
    /// error one if a timeout occurred or some other failure happened.
    #[cfg(feature = "subscriber")]
    pub fn subscribe<F>(&mut self, topic: Uri, handler: F) ->
        Result<impl Future<Item = Subscription, Error = Error>, Error>
        where F: 'static + (Fn(Broadcast) -> Box<Future<Item = (), Error = Error>>) + Send {

        if *self.state.read() != ClientState::Established {
            Err(WampError::InvalidClientState.into())
        } else if !self.router_capabilities.broker {
            Err(WampError::RouterSupportMissing.into())
        } else {
            Ok(subscribe::SubscriptionFuture::new(
                self,
                topic,
                BroadcastHandler { target: Box::new(handler) }
            ))
        }
    }

    //#[cfg(feature = "subscriber")]
    ///// Unsubscribes from a channel.
    //fn unsubscribe(&mut self, subscription: Subscription) -> impl Future<Item=(), Error=Error> {
    //    unimplemented!()
    //}

    /// Closes the connection to the server. The returned future resolves with success if
    /// the connected router actively acknowledges our disconnect within the shutdown_timeout,
    /// and resolves with an error otherwise. After this is called, all incoming messages except
    /// acknowledgement of our disconnection is ignored.
    pub fn close(&mut self, reason: Uri) -> impl Future<Item = (), Error = Error> {
        if *self.state.read() != ClientState::Established {
            Either::A(future::err(WampError::InvalidClientState.into()))
        } else {
            *self.state.write() = ClientState::ShuttingDown;
            Either::B(close::CloseFuture::new(self, reason))
        }
    }
}
impl<T: Transport> Drop for Client<T> {
    fn drop(&mut self) {
        if *self.state.read() != ClientState::Closed {
            error!("Client was not closed before being dropped!");
        }
    }
}

#[derive(Debug)]
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
    pub arguments: Vec<TV>,
    /// The keyword arguments. If there are none, the `HashMap` will be empty.
    pub arguments_kw: HashMap<String, TV>,
}

#[cfg(any(feature = "caller", feature = "callee"))]
/// Return value from an RPC call.
pub struct RpcReturn {
    /// Positional return values. If there are none, set this to an empty `Vec`.
    pub arguments: Vec<TV>,
    /// Keyword return values. If there are none, set this to an empty `HashMap`.
    pub arguments_kw: HashMap<String, TV>,
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
    pub arguments: Vec<TV>,
    /// Keyword values. If there are none, this should be an empty `HashMap`.
    pub arguments_kw: HashMap<String, TV>,
}
