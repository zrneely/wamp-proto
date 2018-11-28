use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use failure::Error;
use futures::{Async, AsyncSink, Future};
use parking_lot::RwLock;
use tokio::timer::Delay;

use client::{Client, ClientState, ClientTaskTracker};
use error::WampError;
use proto::TxMessage;
use {ReceivedValues, Transport, Uri};

#[derive(Debug)]
enum CloseFutureState {
    StartSendGoodbye(Option<TxMessage>),
    SendGoodbye,
    WaitGoodbye,
}

pub(in client) struct CloseFuture<T: Transport> {
    state: CloseFutureState,

    timeout: Delay,

    received: ReceivedValues,
    client_state: Arc<RwLock<ClientState>>,
    task_tracker: Arc<ClientTaskTracker<T>>,
}
impl<T> CloseFuture<T>
where
    T: Transport,
{
    pub fn new(client: &Client<T>, reason: Uri) -> Self {
        CloseFuture {
            state: CloseFutureState::StartSendGoodbye(Some(TxMessage::Goodbye {
                details: HashMap::new(),
                reason,
            })),

            timeout: Delay::new(Instant::now() + client.shutdown_timeout_duration),

            received: client.received.clone(),
            client_state: client.state.clone(),
            task_tracker: client.task_tracker.clone(),
        }
    }
}
impl<T> Future for CloseFuture<T>
where
    T: Transport,
{
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Result<Async<Self::Item>, Self::Error> {
        loop {
            trace!("CloseFuture: {:?}", self.state);
            ::client::check_for_timeout(&mut self.timeout)?;

            match *self.client_state.read() {
                ClientState::ShuttingDown => {}
                ref state => {
                    error!("CloseFuture with unexpected client state {:?}", state);
                    return Err(WampError::InvalidClientState.into());
                }
            }

            let mut pending = false;
            self.state = match self.state {
                // Step 1: Add the goodbye message to the sender's message queue. If the queue is full,
                // return NotReady.
                CloseFutureState::StartSendGoodbye(ref mut message) => {
                    let message = message.take().expect("invalid CloseFutureState");
                    match self.task_tracker.get_sender().lock().start_send(message)? {
                        AsyncSink::NotReady(message) => {
                            pending = true;
                            CloseFutureState::StartSendGoodbye(Some(message))
                        }
                        AsyncSink::Ready => CloseFutureState::SendGoodbye,
                    }
                }

                // Step 2: Wait for the sender's message queue to empty. If it's not empty, return NotReady.
                CloseFutureState::SendGoodbye => match self.task_tracker.get_sender().lock().poll_complete()? {
                    Async::NotReady => {
                        pending = true;
                        CloseFutureState::SendGoodbye
                    }
                    Async::Ready(_) => CloseFutureState::WaitGoodbye,
                },

                // Step 3: Wait for the goodbye response from the router.
                CloseFutureState::WaitGoodbye => {
                    match self.received.goodbye.lock().poll_take(|_| true) {
                        Async::NotReady => {
                            pending = true;
                            CloseFutureState::WaitGoodbye
                        }
                        Async::Ready(msg) => {
                            info!(
                                "WAMP session closed: response {:?} ({:?})",
                                msg.details, msg.reason
                            );
                            *self.client_state.write() = ClientState::Closed;
                            return Ok(Async::Ready(()));
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
