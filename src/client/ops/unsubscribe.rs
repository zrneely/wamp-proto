use std::sync::Arc;
use std::time::Instant;

use failure::Error;
use futures::{Async, AsyncSink, Future};
use tokio::timer::Delay;

use client::{Client, ClientState, ClientTaskTracker};
use error::WampError;
use pollable::PollableValue;
use proto::TxMessage;
use {Id, ReceivedValues, RouterScope, SessionScope, Transport};

#[derive(Debug)]
enum UnsubscribeFutureState {
    StartSendUnsubscribe,
    SendUnsubscribe,
    WaitUnsubscribed,
}

/// A future representing a completed unsubscription.
pub(in client) struct UnsubscriptionFuture<T: Transport> {
    state: UnsubscribeFutureState,

    subscription: Id<RouterScope>,
    request: Id<SessionScope>,
    timeout: Delay,

    received: ReceivedValues,
    client_state: PollableValue<ClientState>,
    task_tracker: Arc<ClientTaskTracker<T>>,
}
impl<T: Transport> UnsubscriptionFuture<T> {
    pub fn new(client: &Client<T>, subscription: Id<RouterScope>) -> Self {
        UnsubscriptionFuture {
            state: UnsubscribeFutureState::StartSendUnsubscribe,

            subscription,
            request: Id::<SessionScope>::next(),
            timeout: Delay::new(Instant::now() + client.timeout_duration),

            received: client.received.clone(),
            client_state: client.state.clone(),
            task_tracker: client.task_tracker.clone(),
        }
    }
}
impl<T: Transport> Future for UnsubscriptionFuture<T> {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Result<Async<Self::Item>, Self::Error> {
        loop {
            trace!("UnsubscribeFuture: {:?}", self.state);
            ::client::check_for_timeout(&mut self.timeout)?;

            match self.client_state.read(true) {
                ClientState::Established => {}
                ref state => {
                    warn!("UnsubscribeFuture with unexpected client state {:?}", state);
                    return Err(WampError::InvalidClientState.into());
                }
            }

            let mut pending = false;
            self.state = match self.state {
                // Step 1: Add the unsubscription request to the sender's message queue. If the queue is full,
                // return NotReady.
                UnsubscribeFutureState::StartSendUnsubscribe => {
                    let message = TxMessage::Unsubscribe {
                        request: self.request,
                        subscription: self.subscription,
                    };
                    match self.task_tracker.get_sender().lock().start_send(message)? {
                        AsyncSink::NotReady(_) => {
                            pending = true;
                            UnsubscribeFutureState::StartSendUnsubscribe
                        }
                        AsyncSink::Ready => UnsubscribeFutureState::SendUnsubscribe,
                    }
                }

                // Step 2: Wait for the sender's message queue to empty.
                UnsubscribeFutureState::SendUnsubscribe => {
                    match self.task_tracker.get_sender().lock().poll_complete()? {
                        Async::NotReady => {
                            pending = true;
                            UnsubscribeFutureState::SendUnsubscribe
                        }
                        Async::Ready(_) => UnsubscribeFutureState::WaitUnsubscribed,
                    }
                }

                // Step 3: Wait for a rx::Unsubscribed message from the receiver.
                UnsubscribeFutureState::WaitUnsubscribed => match self
                    .received
                    .unsubscribed
                    .lock()
                    .poll_take(|msg| msg.request == self.request)
                {
                    Async::NotReady => {
                        pending = true;
                        UnsubscribeFutureState::WaitUnsubscribed
                    }
                    Async::Ready(_) => {
                        // The router has acknowledged our unsubscription; stop the task that listens for
                        // incoming events for the subscription we just cancelled.
                        self.task_tracker.stop_subscription(self.subscription);
                        return Ok(Async::Ready(()));
                    }
                },
            };

            if pending {
                return Ok(Async::NotReady);
            }
        }
    }
}
