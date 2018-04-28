
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use failure::Error;
use futures::Future;
use parking_lot::Mutex;
use tokio_timer::timer;

use {Id, GlobalScope, ReceivedValues, RouterScope, Uri, TransportableValue as TV, Transport};
use error::WampError;

mod initialize;
#[cfg(feature = "subscriber")]
mod subscribe;

#[cfg(feature = "subscriber")]
pub use self::subscribe::Subscription;

use self::subscribe::BroadcastHandler;

/// A WAMP client.
///
/// By default, this supports all 4 roles supported by this crate. They can be
/// selectively enabled if not all necessary (to improve compilation time) with the Cargo
/// features `callee`, `caller`, `publisher`, and `subscriber`.
pub struct Client<T: Transport> {
    sender: Arc<Mutex<T>>,
    received: ReceivedValues,

    session: Id<GlobalScope>,
    timer_handle: timer::Handle,
    timeout_duration: Duration,
    router_capabilities: RouterCapabilities,

    #[cfg(feature = "subscriber")]
    subscriptions: Arc<Mutex<HashMap<Subscription, BroadcastHandler>>>,
}
impl <T: Transport> Client<T> {
    /// Begins initialization of a new [`Client`]. The client will attempt to connect to the WAMP
    /// router at the given URL and join the given realm; the future will not resolve until
    /// both of those tasks succeed.
    ///
    /// This method will handle initialization of the [`Transport`] used, but the caller must
    /// specify which transport will be used.
    ///
    /// The given [`Duration`] will be used as a timeout for protocol-level communications. In
    /// particular, it will *not* be used as a timeout for remote procedure calls. RPC timeouts
    /// can be specified in the [`Client::call`] method, if the `caller` Cargo feature is enabled
    /// (on by default).
    pub fn new(url: &str, realm: Uri, timer_handle: timer::Handle, timeout: Duration) -> impl Future<Item=Self, Error=Error> {
        let received: ReceivedValues = Default::default();
        let (connect, mht) = T::connect(url, received.clone());
        connect.and_then(move |transport| {
            initialize::InitializeFuture::new(transport, received, realm, timer_handle, timeout)
        })
        // TODO also give back mht
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

    #[cfg(feature = "subscriber")]
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
    pub fn subscribe<F: 'static + FnMut(Broadcast) -> Box<Future<Item = (), Error = Error>>>(
        &mut self,
        topic: Uri,
        handler: F
    ) -> Result<subscribe::SubscriptionFuture<T>, Error> {
        if !self.router_capabilities.broker {
            Err(WampError::RouterSupportMissing.into())
        } else {
            Ok(subscribe::SubscriptionFuture::new(self, topic, Box::new(handler)))
        }
    }

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
