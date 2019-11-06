use std::collections::HashMap;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use failure::Error;
use futures::{future::poll_fn, SinkExt};
use tokio::sync::oneshot;

use crate::{
    error::WampError,
    pollable::PollableValue,
    transport::{Transport, TransportableValue as TV},
    uri::Uri,
    GlobalScope, Id, MessageBuffer, RouterScope,
};

mod message_listener;
mod ops {
    pub mod close;
    pub mod initialize;

    #[cfg(feature = "subscriber")]
    pub mod subscribe;
    #[cfg(feature = "subscriber")]
    pub mod unsubscribe;

    #[cfg(feature = "publisher")]
    pub mod publish;
}

// The states a client can be in, according to the WAMP specification.
// Clients are not made available to consumers of this library until they reach
// their initial "Established" state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
    Established {
        session_id: Id<GlobalScope>,
        router_capabilities: RouterCapabilities,
    },
    // The client initiated a clean connection termination, and the router has not yet responded.
    ShuttingDown,
    // The router initiated a clean connection termination, and the client has not yet responded.
    Closing,
    // We're completely done; the transport is closed.
    TransportClosed,
}
impl ClientState {
    pub fn is_established(&self) -> bool {
        match self {
            ClientState::Established { .. } => true,
            _ => false,
        }
    }

    pub fn get_name(&self) -> &'static str {
        match self {
            ClientState::Closed => "CLOSED",
            ClientState::Establishing => "ESTABLISHING",
            ClientState::Failed => "FAILED",
            ClientState::Authenticating => "AUTHENTICATING",
            ClientState::Established { .. } => "ESTABLISHED",
            ClientState::ShuttingDown => "SHUTTING_DOWN",
            ClientState::Closing => "CLOSING",
            ClientState::TransportClosed => "TRANSPORT_CLOSED",
        }
    }
}

/// If the client state ever changes to a value that fails the predicate, this
/// will complete with an error.
async fn watch_for_client_state_change<F, T>(
    state: PollableValue<ClientState>,
    mut is_ok: F,
) -> Result<T, WampError>
where
    F: FnMut(ClientState) -> bool,
{
    poll_fn(move |cx| {
        let state = state.read(Some(cx));
        if is_ok(state) {
            Poll::Pending
        } else {
            error!("unexpected client state: {:?}", state);
            Poll::Ready(Err(WampError::ClientStateChanged(state.get_name())))
        }
    })
    .await
}

type StopSender = oneshot::Sender<()>;
// Tracks all of the tasks owned by a client.
struct ClientTaskTracker<T: Transport> {
    sender: tokio::sync::Mutex<T::Sink>,

    // Used to tell the message listener to stop when a locally-initiated shutdown occurs.
    msg_listener_stop_sender: parking_lot::Mutex<Option<StopSender>>,

    #[cfg(feature = "subscriber")]
    subscriptions: parking_lot::Mutex<HashMap<Id<RouterScope>, StopSender>>,
}
impl<T: Transport> ClientTaskTracker<T> {
    fn new(sender: T::Sink, proto_msg_stop_sender: StopSender) -> Arc<Self> {
        Arc::new(ClientTaskTracker {
            sender: tokio::sync::Mutex::new(sender),
            msg_listener_stop_sender: parking_lot::Mutex::new(Some(proto_msg_stop_sender)),

            #[cfg(feature = "subscriber")]
            subscriptions: parking_lot::Mutex::new(HashMap::new()),
        })
    }

    fn get_sender(&self) -> &tokio::sync::Mutex<T::Sink> {
        &self.sender
    }

    fn stop_message_listener(&self) {
        if let Some(sender) = self.msg_listener_stop_sender.lock().take() {
            let _ = sender.send(());
        }
    }

    #[cfg(feature = "subscriber")]
    fn track_subscription(&self, id: Id<RouterScope>, sender: StopSender) {
        let mut subs = self.subscriptions.lock();
        subs.insert(id, sender);
    }

    #[cfg(feature = "subscriber")]
    fn stop_all_subscriptions(&self) {
        let mut subs = self.subscriptions.lock();
        for (_, sender) in subs.drain() {
            let _ = sender.send(());
        }
    }

    #[cfg(feature = "subscriber")]
    fn stop_subscription(&self, id: Id<RouterScope>) {
        let mut subs = self.subscriptions.lock();
        if let Some(sender) = subs.remove(&id) {
            let _ = sender.send(());
        }
    }

    fn stop_all_except_message_listener(&self) {
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
    /// The user-agent string that the client will optionally send to the router.
    pub user_agent: Option<String>,
}
impl<'a> ClientConfig<'a> {
    /// Creates a new ClientConfig with some default values. They can be overridden after creation if
    /// desired. The defaults are:
    ///
    /// * `timeout`: 10 seconds
    /// * `shutdown_timeout`: 1 second
    /// * `panic_on_drop_while_open`: true
    /// * `user_agent`: None
    pub fn new(url: &'a str, realm: Uri) -> Self {
        ClientConfig {
            url,
            realm,
            timeout: Duration::from_secs(10),
            shutdown_timeout: Duration::from_secs(1),
            panic_on_drop_while_open: true,
            user_agent: None,
        }
    }
}

// TODO: move to subscribe.rs and re-export it here.
/// An extension on top of a Stream that is used by subscriptions.
#[cfg(feature = "subscriber")]
pub trait SubscriptionStream: tokio::stream::Stream<Item = Broadcast> {
    /// Returns the ID of this subscription.
    fn get_subscription_id(&self) -> Id<RouterScope>;
}

/// A WAMP client.
///
/// By default, this supports all 4 roles supported by this crate. They can be
/// selectively enabled if not all necessary (to improve compilation time) with the Cargo
/// features `callee`, `caller`, `publisher`, and `subscriber`.
pub struct Client<T: Transport> {
    received: Arc<MessageBuffer>,

    // TODO: support timeouts

    // All operations should check the state before accepting incoming messages
    state: PollableValue<ClientState>,
    task_tracker: Arc<ClientTaskTracker<T>>,

    panic_on_drop_while_open: bool,
}
impl<T: Transport> std::fmt::Debug for Client<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("received", &self.received)
            .field("state", &self.state)
            .field("panic_on_drop_while_open", &self.panic_on_drop_while_open)
            .finish()
    }
}
impl<T: 'static + Transport> Client<T> {
    /// Begins initialization of a new [`Client`]. See [`ClientConfig`] for details.
    ///
    /// The client will attempt to connect to the WAMP router at the given URL and join the given
    /// realm; the future will not resolve until both of those tasks succeed and the connection
    /// is established.
    ///
    /// This method will handle initialization of the [`Transport`] used, but the caller must
    /// specify the type of transport.
    pub async fn new(config: ClientConfig<'_>) -> Result<Self, WampError> {
        let ClientConfig {
            url,
            realm,
            panic_on_drop_while_open,
            user_agent,
            ..
        } = config;
        let (sink, stream) = T::connect(url).await.map_err(WampError::ConnectFailed)?;

        ops::initialize::initialize(sink, stream, realm, panic_on_drop_while_open, user_agent).await
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

    /// Publishes a message. The returned future will complete as soon as the protocol-level work
    /// of sending the message is complete.
    ///
    /// Note that in particular, if publishing the message fails because the broker elects to
    /// refuse it, that failure will not be reflected in the [`Result`] this returns.
    #[cfg(feature = "publisher")]
    pub async fn publish(&mut self, topic: Uri, broadcast: Broadcast) -> Result<(), WampError> {
        match self.state.read(None) {
            ClientState::Established {
                router_capabilities,
                ..
            } => {
                if router_capabilities.broker {
                    ops::publish::publish(self, topic, broadcast).await
                } else {
                    Err(WampError::RouterSupportMissing("broker"))
                }
            }
            state => {
                warn!("Tried to publish when client state was {:?}", state);
                Err(WampError::InvalidClientState)
            }
        }
    }

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
    /// # Return Value
    ///
    /// This method provides a [`Stream`] which will provide the [`Broadcast`]s that are received on the
    /// channel.
    ///
    /// To cancel the subscription, you should call the unsubscribe method, passing in the stream
    /// provided by this method. Simply dropping the stream is NOT sufficient.
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
    pub async fn subscribe(&mut self, topic: Uri) -> Result<impl SubscriptionStream, WampError> {
        match self.state.read(None) {
            ClientState::Established {
                router_capabilities,
                ..
            } => {
                if router_capabilities.broker {
                    ops::subscribe::subscribe(self, topic).await
                } else {
                    Err(WampError::RouterSupportMissing("broker"))
                }
            }
            state => {
                warn!("Tried to subscribe when client state was {:?}", state);
                Err(WampError::InvalidClientState)
            }
        }
    }

    /// Cancels an existing subscription.
    #[cfg(feature = "subscriber")]
    pub async fn unsubscribe<S: SubscriptionStream>(
        &mut self,
        subscription: S,
    ) -> Result<(), WampError> {
        match self.state.read(None) {
            ClientState::Established {
                router_capabilities,
                ..
            } => {
                if router_capabilities.broker {
                    ops::unsubscribe::unsubscribe(self, subscription).await
                } else {
                    Err(WampError::RouterSupportMissing("broker"))
                }
            }
            state => {
                warn!("Tried to unsubscribe when client state was {:?}", state);
                Err(WampError::InvalidClientState)
            }
        }
    }

    /// Closes the connection to the server. The returned future resolves with success if
    /// the connected router actively acknowledges our disconnect within the shutdown_timeout,
    /// and resolves with an error otherwise. After this is called (but before it resolves), all
    /// incoming messages except acknowledgement of our disconnection are ignored.
    pub async fn close(&mut self, reason: Uri) -> Result<(), Error> {
        self.state.write(ClientState::ShuttingDown);

        // Stop listening for incoming Pub/Sub events and RPC invocations.
        self.task_tracker.stop_all_except_message_listener();

        let task_tracker = self.task_tracker.clone();
        let state = self.state.clone();
        ops::close::close(self, reason).await?;

        // We've entered the Closed state now that CloseFuture is resolved.
        // Stop listening for incoming ABORT and GOODBYE messages and begin
        // closing the transport.
        task_tracker.stop_all_except_message_listener();
        task_tracker.get_sender().lock().await.close().await?;

        state.write(ClientState::TransportClosed);
        Ok(())
    }

    /// Returns true if the client is, at the instant of querying, in the TransportClosed state.
    pub fn is_transport_closed(&self) -> bool {
        self.state.read(None) == ClientState::TransportClosed
    }

    /// Waits for the client to enter the `TransportClosed` state.
    pub async fn wait_for_close(&self) {
        poll_fn(|cx| match self.state.read(Some(cx)) {
            ClientState::TransportClosed => Poll::Ready(()),
            _ => Poll::Pending,
        })
        .await
    }
}
impl<T: Transport> Drop for Client<T> {
    fn drop(&mut self) {
        // Make a best-effort attempt to stop everything.
        self.task_tracker.stop_all_except_message_listener();
        self.task_tracker.stop_message_listener();

        let state = self.state.read(None);
        if state != ClientState::TransportClosed {
            error!(
                "Client was not completely closed before being dropped (actual state: {:?})!",
                state
            );

            if self.panic_on_drop_while_open {
                panic!("Client was not completely closed before being dropped!");
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
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

/// Arguments to an RPC call.
#[cfg(any(feature = "caller", feature = "callee"))]
#[derive(Debug)]
pub struct RpcArgs {
    /// The positional arguments. If there are none, the `Vec` will be empty.
    pub arguments: Vec<TV>,
    /// The keyword arguments. If there are none, the `HashMap` will be empty.
    pub arguments_kw: HashMap<String, TV>,
}

/// Return value from an RPC call.
#[cfg(any(feature = "caller", feature = "callee"))]
#[derive(Debug)]
pub struct RpcReturn {
    /// Positional return values. If there are none, set this to an empty `Vec`.
    pub arguments: Vec<TV>,
    /// Keyword return values. If there are none, set this to an empty `HashMap`.
    pub arguments_kw: HashMap<String, TV>,
}

/// A message broadcast on a channel.
#[cfg(any(feautre = "publisher", feature = "subscriber"))]
#[derive(Debug)]
pub struct Broadcast {
    /// Positional values. If there are none, this should be an empty `Vec`.
    pub arguments: Vec<TV>,
    /// Keyword values. If there are none, this should be an empty `HashMap`.
    pub arguments_kw: HashMap<String, TV>,
}
