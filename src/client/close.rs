
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use failure::Error;
use futures::{Async, AsyncSink, Future};
use parking_lot::{Mutex, RwLock};
use tokio::timer::Delay;

use {ReceivedValues, Transport, Uri};
use client::{Client, ClientState};
use proto::TxMessage;

#[derive(Debug)]
enum CloseFutureState {
    StartSendGoodbye(Option<TxMessage>),
    SendGoodbye,
    WaitGoodbye,
}

pub(super) struct CloseFuture<T: Transport> {
    state: CloseFutureState,

    timeout: Delay,

    sender: Arc<Mutex<T>>,
    received: ReceivedValues,
    client_state: Arc<RwLock<ClientState>>,
}
impl<T> CloseFuture<T> where T: Transport {
    pub fn new(client: &mut Client<T>, reason: Uri) -> Self {
        CloseFuture {
            state: CloseFutureState::StartSendGoodbye(Some(TxMessage::Goodbye {
                details: HashMap::new(),
                reason,
            })),

            timeout: Delay::new(Instant::now() + client.shutdown_timeout_duration),

            sender: client.sender.clone(),
            received: client.received.clone(),
            client_state: client.state.clone(),
        }
    }
}
impl<T> Future for CloseFuture<T> where T: Transport {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Result<Async<Self::Item>, Self::Error> {
        loop {
            trace!("CloseFuture: {:?}", self.state);
            super::check_for_timeout_or_error(&mut self.timeout, &mut self.received)?;

            let mut pending = false;
            self.state = match self.state {
                // Step 1: Add the goodbye message to the sender's message queue. If the queue is full,
                // return NotReady.
                CloseFutureState::StartSendGoodbye(ref mut message) => {
                    let message = message.take().expect("invalid CloseFutureState");
                    match self.sender.lock().start_send(message)? {
                        AsyncSink::NotReady(message) => {
                            pending = true;
                            CloseFutureState::StartSendGoodbye(Some(message))
                        }
                        AsyncSink::Ready => CloseFutureState::SendGoodbye,
                    }
                }

                // Step 2: Wait for the sender's message queue to empty. If it's not empty, return NotReady.
                CloseFutureState::SendGoodbye => {
                    match self.sender.lock().poll_complete()? {
                        Async::NotReady => {
                            pending = true;
                            CloseFutureState::SendGoodbye
                        }
                        Async::Ready(_) => CloseFutureState::WaitGoodbye,
                    }
                }

                // Step 3: Wait for the goodbye response from the router.
                CloseFutureState::WaitGoodbye => {
                    match self.received.goodbye.lock().poll_take(|_| true) {
                        Async::NotReady => {
                            pending = true;
                            CloseFutureState::WaitGoodbye
                        }
                        Async::Ready(msg) => {
                            info!("WAMP session closed: response {:?} ({:?})", msg.details, msg.reason);
                            *self.client_state.write() = ClientState::Closed;
                            return Ok(Async::Ready(()))
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