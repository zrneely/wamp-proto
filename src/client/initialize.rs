use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use failure::Error;
use futures::{sync::oneshot, Async, AsyncSink, Future};
use parking_lot::{Mutex, RwLock};
use tokio::timer::Delay;

use client::{Client, ClientState, RouterCapabilities};
use proto::TxMessage;
use uri::{known_uri, Uri};
use {ReceivedValues, Transport, TransportableValue as TV};

#[derive(Debug)]
enum ProtocolMessageListenerState {
    Ready,
    StartReplyGoodbye,
    SendGoodbye,
    StopAllTasks,
}

// Used to listen for ABORT and GOODBYE messages from the router.
struct ProtocolMessageListener<T: Transport> {
    sender: Arc<Mutex<T>>,
    values: ReceivedValues,
    client_state: Arc<RwLock<ClientState>>,

    state: ProtocolMessageListenerState,
    stop_receiver: oneshot::Receiver<()>,
}
impl<T: Transport> Future for ProtocolMessageListener<T> {
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Result<Async<Self::Item>, Self::Error> {
        trace!("ProtocolMessageListener wakeup");
        loop {
            match self.stop_receiver.poll() {
                // We haven't been told to stop
                Ok(Async::NotReady) => {}
                // Either we've been told to stop or the sender was dropped,
                // in which case we should stop anyway.
                Ok(Async::Ready(_)) | Err(_) => {
                    debug!("ProtocolMessageListener told to stop!");
                    return Ok(Async::Ready(()));
                }
            }

            let mut pending = false;
            self.state = match self.state {
                ProtocolMessageListenerState::Ready => {
                    // Poll for ABORT
                    match self.values.abort.lock().poll_take(|_| true) {
                        Async::Ready(msg) => {
                            warn!(
                                "Received ABORT from router: \"{:?}\" ({:?})",
                                msg.reason, msg.details
                            );
                            *self.client_state.write() = ClientState::Closed;
                            ProtocolMessageListenerState::StopAllTasks
                        }

                        // Poll for GOODBYE
                        Async::NotReady => match self.values.goodbye.lock().poll_take(|_| true) {
                            Async::Ready(msg) => {
                                info!(
                                    "Received GOODBYE from router: \"{:?}\" ({:?})",
                                    msg.reason, msg.details
                                );
                                *self.client_state.write() = ClientState::Closing;
                                ProtocolMessageListenerState::StartReplyGoodbye
                            }
                            Async::NotReady => {
                                pending = true;
                                ProtocolMessageListenerState::Ready
                            }
                        },
                    }
                }

                ProtocolMessageListenerState::StartReplyGoodbye => {
                    let message = TxMessage::Goodbye {
                        details: HashMap::new(),
                        reason: Uri::raw(known_uri::session_close::goodbye_and_out.to_string()),
                    };
                    debug!(
                        "ProtocolMessageListenerState sending GOODBYE response: {:?}",
                        message
                    );
                    match self.sender.lock().start_send(message) {
                        Ok(AsyncSink::NotReady(_)) => {
                            pending = true;
                            ProtocolMessageListenerState::StartReplyGoodbye
                        }
                        Ok(AsyncSink::Ready) => ProtocolMessageListenerState::SendGoodbye,
                        Err(e) => {
                            error!("ProtocolMessageListener got err {:?} while initiating GOODBYE response!", e);
                            return Err(());
                        }
                    }
                }
                ProtocolMessageListenerState::SendGoodbye => {
                    match self.sender.lock().poll_complete() {
                        Ok(Async::NotReady) => {
                            pending = true;
                            ProtocolMessageListenerState::SendGoodbye
                        }
                        Ok(Async::Ready(_)) => ProtocolMessageListenerState::StopAllTasks,
                        Err(e) => {
                            error!("ProtocolMessageListener got err {:?} while flushing GOODBYE response!", e);
                            return Err(());
                        }
                    }
                }

                ProtocolMessageListenerState::StopAllTasks => {
                    info!("ProtocolMessageListener stopping all tasks!");
                    // TODO: somehow stop all tasks. Do we just have to store stop signal Senders
                    // for everything here? They are Send + Sync so that's possible. It would be more
                    // convenient for all the tasks to live in one "ClientTaskTracker" struct or something.
                    return Ok(Async::Ready(()));
                }
            };

            if pending {
                return Ok(Async::NotReady);
            }
        }
    }
}

#[derive(Debug)]
enum InitializeFutureState {
    StartSendHello(Option<TxMessage>),
    SendHello,
    WaitWelcome,
}

pub(super) struct InitializeFuture<T: Transport + 'static> {
    state: InitializeFutureState,
    timeout: Delay,

    // client properties
    timeout_duration: Duration,
    shutdown_timeout_duration: Duration,
    panic_on_drop_while_open: bool,

    sender: Arc<Mutex<T>>,
    received: ReceivedValues,
}
impl<T> InitializeFuture<T>
where
    T: Transport + 'static,
{
    pub(crate) fn new(
        mut sender: T,
        received: ReceivedValues,
        realm: Uri,
        timeout_duration: Duration,
        shutdown_timeout_duration: Duration,
        panic_on_drop_while_open: bool,
    ) -> Self {
        let timeout = Delay::new(Instant::now() + timeout_duration);
        sender.listen();

        InitializeFuture {
            state: InitializeFutureState::StartSendHello(Some(TxMessage::Hello {
                realm: realm.clone(),
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
            })),

            timeout,
            timeout_duration,
            shutdown_timeout_duration,
            panic_on_drop_while_open,

            sender: Arc::new(Mutex::new(sender)),
            received,
        }
    }
}
impl<T> Future for InitializeFuture<T>
where
    T: Transport + 'static,
{
    type Item = Client<T>;
    type Error = Error;

    fn poll(&mut self) -> Result<Async<Self::Item>, Self::Error> {
        loop {
            trace!("InitializeFuture: {:?}", self.state);
            super::check_for_timeout(&mut self.timeout)?;

            let mut pending = false;
            self.state = match self.state {
                // Step 1: Add the hello message to the sender's message queue.
                InitializeFutureState::StartSendHello(ref mut message) => {
                    let message = message.take().expect("invalid InitializeFutureState");
                    match self.sender.lock().start_send(message)? {
                        AsyncSink::NotReady(message) => {
                            pending = true;
                            InitializeFutureState::StartSendHello(Some(message))
                        }
                        AsyncSink::Ready => InitializeFutureState::SendHello,
                    }
                }

                // Step 2: Wait for the sender's message queue to empty.
                InitializeFutureState::SendHello => match self.sender.lock().poll_complete()? {
                    Async::NotReady => {
                        pending = true;
                        InitializeFutureState::SendHello
                    }
                    Async::Ready(_) => InitializeFutureState::WaitWelcome,
                },

                // Step 3: Wait for a rx::Welcome message.
                InitializeFutureState::WaitWelcome => {
                    match self.received.welcome.lock().poll_take(|_| true) {
                        Async::NotReady => {
                            pending = true;
                            InitializeFutureState::WaitWelcome
                        }
                        Async::Ready(msg) => {
                            info!(
                                "WAMP connection established with session ID {:?}",
                                msg.session
                            );
                            let client_state = Arc::new(RwLock::new(ClientState::Established));

                            let (sender, receiver) = oneshot::channel();
                            let proto_msg_listener = ProtocolMessageListener {
                                sender: self.sender.clone(),
                                values: self.received.clone(),
                                client_state: client_state.clone(),
                                state: ProtocolMessageListenerState::Ready,
                                stop_receiver: receiver,
                            };
                            tokio::spawn(proto_msg_listener);

                            return Ok(Async::Ready(Client {
                                sender: self.sender.clone(),
                                received: self.received.clone(),
                                proto_msg_stop_sender: Some(sender),

                                session_id: msg.session,
                                timeout_duration: self.timeout_duration,
                                shutdown_timeout_duration: self.shutdown_timeout_duration,
                                panic_on_drop_while_open: self.panic_on_drop_while_open,
                                router_capabilities: RouterCapabilities::from_details(&msg.details),

                                // We've already sent our "hello" and received our "welcome".
                                state: client_state,

                                #[cfg(feature = "subscriber")]
                                subscriptions: Arc::new(Mutex::new(HashMap::new())),
                            }));
                        }
                    }
                }
            };

            if pending {
                return Ok(Async::NotReady);
            }
        }
    }
}
