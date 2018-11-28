use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use failure::Error;
use futures::{
    future::{self, Either, Future},
    sync::oneshot,
    Async,
};
use parking_lot::{Mutex, RwLock};
use tokio::timer::Delay;

use error::WampError;
use {
    ConnectResult, GlobalScope, Id, ReceivedValues, RouterScope, Transport,
    TransportableValue as TV, Uri,
};

mod ops {
    pub mod close;
    pub mod initialize;

    #[cfg(feature = "subscriber")]
    pub mod subscribe;
    #[cfg(feature = "subscriber")]
    pub mod unsubscribe;
}

// helper for most operations
fn check_for_timeout(timeout: &mut Delay) -> Result<(), Error> {
    match timeout.poll()? {
        Async::Ready(_) => {
            info!("Timeout detected!");
            return Err(WampError::Timeout.into());
        }
        _ => {}
    };
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
    // We're completely done; the transport is closed.
    TransportClosed,
}

type StopSender = oneshot::Sender<()>;
// Tracks all of the tasks owned by a client. This is Send + Sync because StopSender is
// Send + Sync. Mutex<T> is Send + Sync as long as T is Send. HashMap<K, V> is Send as long
// as K and V are both Send.
// TODO: rename to ClientResourceHandle or something
struct ClientTaskTracker<T: Transport> {
    sender: Mutex<T>,
    proto_msg_stop_sender: Mutex<Option<StopSender>>,

    #[cfg(feature = "subscriber")]
    subscriptions: Mutex<HashMap<Id<RouterScope>, StopSender>>,
}
impl<T> ClientTaskTracker<T> where T: Transport {
    fn new(sender: T, proto_msg_stop_sender: StopSender) -> Arc<Self> {
        Arc::new(ClientTaskTracker {
            sender: Mutex::new(sender),
            proto_msg_stop_sender: Mutex::new(Some(proto_msg_stop_sender)),

            #[cfg(feature = "subscriber")]
            subscriptions: Mutex::new(HashMap::new()),
        })
    }

    fn get_sender(&self) -> &Mutex<T> {
        &self.sender
    }

    fn stop_proto_msg_listener(&self) {
        if let Some(sender) = self.proto_msg_stop_sender.lock().take() {
            let _ = sender.send(());
        }
    }

    fn close_transport(&self) -> T::CloseFuture {
        let mut lock = self.sender.lock();
        Transport::close(&mut *lock)
    }

    #[cfg(feature = "subscriber")]
    fn track_subscription(&self, id: Id<RouterScope>, sender: StopSender) {
        let mut subs = self.subscriptions.lock();
        subs.insert(id, sender);
    }

    #[cfg(feature = "subscriber")]
    fn stop_all_subscriptions(&self) {
        let mut subs = self.subscriptions.lock();
        for (_, mut sender) in subs.drain() {
            let _ = sender.send(());
        }
    }

    #[cfg(feature = "subscriber")]
    fn stop_subscription(&self, id: &Id<RouterScope>) {
        let mut subs = self.subscriptions.lock();
        if let Some(sender) = subs.remove(id) {
            let _ = sender.send(());
        }
    }

    fn stop_all_except_proto_msg(&self) {
        #[cfg(feature = "subscriber")]
        self.stop_all_subscriptions();
    }
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
    /// If true, the client will panic when dropped if it's still open.
    pub panic_on_drop_while_open: bool,
}
impl<'a> ClientConfig<'a> {
    /// Creates a new ClientConfig with some default values. They can be overridden after creation if
    /// desired. The defaults are:
    ///
    /// * `timeout`: 10 seconds
    /// * `shutdown_timeout`: 1 second
    /// * `panic_on_drop_while_open`: true
    pub fn new(url: &'a str, realm: Uri) -> Self {
        ClientConfig {
            url,
            realm,
            timeout: Duration::from_secs(10),
            shutdown_timeout: Duration::from_secs(1),
            panic_on_drop_while_open: true,
        }
    }
}

/// A WAMP client.
///
/// By default, this supports all 4 roles supported by this crate. They can be
/// selectively enabled if not all necessary (to improve compilation time) with the Cargo
/// features `callee`, `caller`, `publisher`, and `subscriber`.
pub struct Client<T: Transport> {
    received: ReceivedValues,

    session_id: Id<GlobalScope>,
    timeout_duration: Duration,
    shutdown_timeout_duration: Duration,
    router_capabilities: RouterCapabilities,

    // All operations should check the state before accepting incoming messages
    state: Arc<RwLock<ClientState>>,
    task_tracker: Arc<ClientTaskTracker<T>>,

    panic_on_drop_while_open: bool,
}
impl<T: Transport> std::fmt::Debug for Client<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "Client {{\n\treceived: {:?}\n\tsession_id: {:?}\n\ttimeout_duration: {:?}\n\trouter_capabilities: {:?}\n}}",
            self.received, self.session_id, self.timeout_duration, self.router_capabilities
        )
    }
}
impl<T: Transport> Client<T> {
    /// Begins initialization of a new [`Client`]. See [`ClientConfig`] for details.
    ///
    /// The client will attempt to connect to the WAMP router at the given URL and join the given
    /// realm; the future will not resolve until both of those tasks succeed and the connection
    /// is established.
    ///
    /// This method will handle initialization of the [`Transport`] used, but the caller must
    /// specify the type of transport.
    pub fn new<'a>(config: ClientConfig<'a>) -> impl Future<Item = Self, Error = Error> {
        let ClientConfig {
            url,
            realm,
            timeout,
            shutdown_timeout,
            panic_on_drop_while_open,
        } = config;
        let ConnectResult {
            future,
            received_values,
        } = T::connect(url);

        future.and_then(move |transport| {
            ops::initialize::InitializeFuture::new(
                transport,
                received_values,
                realm,
                timeout,
                shutdown_timeout,
                panic_on_drop_while_open,
            )
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
    /// This method returns a future which will resolve to success once the subscription is
    /// acknowledged by the router. The value returned from the future is the subscription ID,
    /// which can be given to [`unsubscribe`] to cancel the subscription.
    ///
    /// # Failure Modes
    ///
    /// * If the client is closed (either because of a local or remote closure), the returned future
    /// will immediately resolve with an error.
    ///
    /// * If the router does not support the "broker" role, the returned future will immediately resolve
    /// with an error.
    ///
    /// * If the router does not acknowledge the subscription within the client timeout period, the returned
    /// future will resolve with an error.
    #[cfg(feature = "subscriber")]
    pub fn subscribe<F>(
        &mut self,
        topic: Uri,
        handler: F,
    ) -> impl Future<Item = Id<RouterScope>, Error = Error>
    where
        F: 'static + Fn(Broadcast) -> Box<Future<Item = (), Error = Error>> + Send,
    {
        if *self.state.read() != ClientState::Established {
            Either::A(future::err(WampError::InvalidClientState.into()))
        } else if !self.router_capabilities.broker {
            Either::A(future::err(WampError::RouterSupportMissing.into()))
        } else {
            Either::B(ops::subscribe::SubscriptionFuture::new(
                self,
                topic,
                BroadcastHandler {
                    target: Box::new(handler),
                },
            ))
        }
    }

    #[cfg(feature = "subscriber")]
    /// Unsubscribes from a channel.
    pub fn unsubscribe(&mut self, subscription: Id<RouterScope>) -> impl Future<Item=(), Error=Error> {
        ops::unsubscribe::UnsubscriptionFuture::new(self, subscription)
    }

    /// Closes the connection to the server. The returned future resolves with success if
    /// the connected router actively acknowledges our disconnect within the shutdown_timeout,
    /// and resolves with an error otherwise. After this is called, all incoming messages except
    /// acknowledgement of our disconnection is ignored.
    pub fn close(&mut self, reason: Uri) -> impl Future<Item = (), Error = Error> {
        // Use anonymous scope to limit the time we have the lock on our state
        {
            let mut state_lock = self.state.write();
            if *state_lock != ClientState::Established {
                return Either::A(future::err(WampError::InvalidClientState.into()));
            } else {
                *state_lock = ClientState::ShuttingDown;
                // Stop listening for incoming events and RPC invocations
                self.task_tracker.stop_all_except_proto_msg();
            }
        }

        let task_tracker = self.task_tracker.clone();
        let state = self.state.clone();
        Either::B(ops::close::CloseFuture::new(self, reason).and_then(move |_| {
            // We've entered the Closed state now that CloseFuture is resolved.
            // Stop listening for incoming ABORT and GOODBYE messages and begin
            // closing the transport.
            task_tracker.stop_proto_msg_listener();
            task_tracker.close_transport()
        }).and_then(move |_| {
            *state.write() = ClientState::TransportClosed;
            future::ok(())
        }))
    }
}
impl<T: Transport> Drop for Client<T> {
    fn drop(&mut self) {
        // Make a best-effort attempt to stop everything.
        self.task_tracker.stop_all_except_proto_msg();
        self.task_tracker.stop_proto_msg_listener();

        let state = self.state.read();
        if *state != ClientState::TransportClosed {
            error!(
                "Client was not completely closed before being dropped (actual state: {:?})!",
                *state
            );

            if self.panic_on_drop_while_open {
                panic!("Client was not completely closed before being dropped!");
            }
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
            },
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
