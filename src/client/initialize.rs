
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use failure::Error;
use futures::{Async, Future, Sink};
use futures::task::Context;
use parking_lot::Mutex;
use tokio_timer::{timer, Delay};

use {Client, ReceivedValues, Transport, TransportableValue as TV, Uri};
use client::RouterCapabilities;
use error::WampError;
use proto::TxMessage;

enum InitializeFutureState {
    StartSendHello(Option<TxMessage>),
    SendHello,
    WaitWelcome,
}

pub(crate) struct InitializeFuture<T: Transport> {
    state: InitializeFutureState,

    realm: Uri,

    timer_handle: timer::Handle,
    timeout: Delay,
    timeout_duration: Duration,

    sender: Arc<Mutex<T>>,
    received: ReceivedValues,
}
impl <T> InitializeFuture<T> where T: Transport {
    pub(crate) fn new(
        sender: T,
        received: ReceivedValues,
        realm: Uri,
        timer_handle: timer::Handle,
        timeout: Duration,
    ) -> Self {
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

            realm,

            timer_handle,
            timeout: timer_handle.delay(Instant::now() + timeout),
            timeout_duration: timeout,

            sender: Arc::new(Mutex::new(sender)),
            received,
        }
    }
}
impl <T> Future for InitializeFuture<T> where T: Transport {
    type Item = Client<T>;
    type Error = Error;

    fn poll(&mut self, cx: &mut Context) -> Result<Async<Self::Item>, Self::Error> {
        loop {

            // Before running the state machine, check for timeout.
            match self.timeout.poll(cx)? {
                Async::Pending => {}
                Async::Ready(_) => return Err(WampError::Timeout {
                    time: self.timeout_duration,
                }.into()),
            }

            match self.state {
                // Step 1: Add the hello message to the sender's message queue.
                InitializeFutureState::StartSendHello(Some(message)) => {
                    let sender = self.sender.lock();
                    match sender.poll_ready(cx)? {
                        Async::Pending => {
                            self.state = InitializeFutureState::StartSendHello(Some(message));
                            return Ok(Async::Pending);
                        }
                        Async::Ready(_) => {
                            self.state = InitializeFutureState::SendHello;
                            sender.start_send(message)?;
                        }
                    }
                }
                InitializeFutureState::StartSendHello(None) => {
                    panic!("invalid InitializeFutureState");
                }

                // Step 2: Wait for the sender's message queue to empty.
                InitializeFutureState::SendHello => {
                    try_ready!(self.sender.lock().poll_flush(cx));
                    self.state = InitializeFutureState::WaitWelcome;
                }

                // Step 3: Wait for a rx::Welcome message.
                InitializeFutureState::WaitWelcome => {
                    let msg = try_ready!(Ok(self.received.welcome.lock().poll_take(|_| true, cx)));

                    return Ok(Async::Ready(Client {
                        sender: self.sender,
                        received: self.received,

                        session: msg.session,
                        timeout_duration: self.timeout_duration,
                        timer_handle: self.timer_handle,
                        router_capabilities: RouterCapabilities::from_details(&msg.details),

                        subscriptions: Arc::new(Mutex::new(HashMap::new())),
                    }))
                }
            }
        }
    }
}
