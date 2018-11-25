
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use failure::Error;
use futures::{Async, AsyncSink, Future};
use parking_lot::Mutex;
use tokio::timer::Delay;

use {ReceivedValues, Transport, TransportableValue as TV, Uri};
use client::{Client, ClientState, RouterCapabilities};
use proto::TxMessage;

enum InitializeFutureState {
    StartSendHello(Option<TxMessage>),
    SendHello,
    WaitWelcome,
}

pub(crate) struct InitializeFuture<T: Transport> {
    state: InitializeFutureState,

    timeout: Delay,
    timeout_duration: Duration,

    sender: Arc<Mutex<T>>,
    received: ReceivedValues,
}
impl <T> InitializeFuture<T> where T: Transport {
    pub(crate) fn new(
        mut sender: T,
        received: ReceivedValues,
        realm: Uri,
        timeout_duration: Duration,
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

            timeout, timeout_duration,

            sender: Arc::new(Mutex::new(sender)),
            received,
        }
    }
}
impl <T> Future for InitializeFuture<T> where T: Transport {
    type Item = Client<T>;
    type Error = Error;

    fn poll(&mut self) -> Result<Async<Self::Item>, Self::Error> {
        loop {
            // Before running the state machine, check for timeout or errors.
            super::check_for_timeout_or_error(&mut self.timeout, &mut self.received)?;

            let mut pending = false;
            self.state = match self.state {
                // Step 1: Add the hello message to the sender's message queue.
                InitializeFutureState::StartSendHello(ref mut message) => {
                    trace!("InitializeFutureState::StartSendHello");
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
                InitializeFutureState::SendHello => {
                    trace!("InitializeFutureState::SendHello");
                    match self.sender.lock().poll_complete()? {
                        Async::NotReady => {
                            pending = true;
                            InitializeFutureState::SendHello
                        }
                        Async::Ready(_) => InitializeFutureState::WaitWelcome
                    }
                }

                // Step 3: Wait for a rx::Welcome message.
                InitializeFutureState::WaitWelcome => {
                    trace!("InitializeFutureState::WaitWelcome");
                    match self.received.welcome.lock().poll_take(|_| true) {
                        Async::NotReady => {
                            pending = true;
                            InitializeFutureState::WaitWelcome
                        }
                        Async::Ready(msg) => {
                            info!("WAMP connection initialized with session ID {:?}", msg.session);
                            return Ok(Async::Ready(Client {
                                sender: self.sender.clone(),
                                received: self.received.clone(),

                                session_id: msg.session,
                                timeout_duration: self.timeout_duration,
                                router_capabilities: RouterCapabilities::from_details(&msg.details),

                                // We've already sent our "hello" and received our "welcome".
                                state: ClientState::Established,

                                subscriptions: Arc::new(Mutex::new(HashMap::new())),
                            }))
                        }
                    }
                }
            };

            if pending {
                return Ok(Async::NotReady)
            }
        }
    }
}
