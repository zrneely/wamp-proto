use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use std::{cmp, fmt, hash};

use failure::Error;
use futures::{sync::oneshot, Async, AsyncSink, Future};
use parking_lot::{Mutex, RwLock};
use tokio::timer::Delay;

use client::{BroadcastHandler, Client, ClientState};
use error::WampError;
use proto::TxMessage;
use {Id, ReceivedValues, RouterScope, SessionScope, Transport, Uri};

/// The result of subscribing to a channel.
pub(super) struct Subscription {
    // The ID of the subscription, chosen by the router.
    subscription_id: Id<RouterScope>,
    /// A handle to tell the associated task to stop listening.
    pub listener_stop_sender: oneshot::Sender<()>,
}
impl fmt::Debug for Subscription {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Subscription {{ subscription_id: {:?} }}",
            self.subscription_id
        )
    }
}
impl cmp::PartialEq for Subscription {
    fn eq(&self, other: &Self) -> bool {
        self.subscription_id == other.subscription_id
    }
}
impl cmp::Eq for Subscription {}
impl hash::Hash for Subscription {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        self.subscription_id.hash(state);
    }
}

// The future (task) that listens for new events published to a channel we're
// subscribed to.
struct SubscriptionListener {
    values: ReceivedValues,
    stop_receiver: oneshot::Receiver<()>,
    subscription_id: Id<RouterScope>,
    handler: BroadcastHandler,
}
impl Future for SubscriptionListener {
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Result<Async<Self::Item>, Self::Error> {
        trace!("SubscriptionListener wakeup");
        loop {
            match self.stop_receiver.poll() {
                // We haven't been told to stop
                Ok(Async::NotReady) => {}
                // Either we've been told to stop, or the sender was dropped, in
                // which case we should stop anyway.
                Ok(Async::Ready(_)) | Err(_) => {
                    debug!("SubscriptionListener told to stop!");
                    return Ok(Async::Ready(()));
                }
            }

            let poll_result = self
                .values
                .event
                .lock()
                .poll_take(|evt| evt.subscription == self.subscription_id);
            match poll_result {
                Async::Ready(event) => {
                    // TODO: convert the event to a "broadcast" object and invoke the callback
                    // TODO: invoke the callback as part of this Future (add state machine), or on its own task?
                    info!(
                        "Subscription {:?} received event: {:?}",
                        self.subscription_id, event
                    );
                    unimplemented!();
                }

                // Nothing available yet
                Async::NotReady => return Ok(Async::NotReady),
            }
        }
    }
}

#[derive(Debug)]
enum SubscriptionFutureState {
    StartSendSubscribe(Option<TxMessage>),
    SendSubscribe,
    WaitSubscribed,
}

/// A future representing a completed subscription.
pub(super) struct SubscriptionFuture<T: Transport> {
    state: SubscriptionFutureState,

    topic: Uri,
    handler: Option<BroadcastHandler>,
    request_id: Id<SessionScope>,
    timeout: Delay,

    sender: Arc<Mutex<T>>,
    received: ReceivedValues,
    subscriptions: Arc<Mutex<HashMap<Id<RouterScope>, Subscription>>>,
    client_state: Arc<RwLock<ClientState>>,
}
impl<T: Transport> SubscriptionFuture<T> {
    pub(super) fn new(client: &mut Client<T>, topic: Uri, handler: BroadcastHandler) -> Self {
        let request_id = Id::<SessionScope>::next();
        SubscriptionFuture {
            state: SubscriptionFutureState::StartSendSubscribe(Some(TxMessage::Subscribe {
                topic: topic.clone(),
                request: request_id,
                options: HashMap::new(),
            })),

            topic,
            request_id,
            handler: Some(handler),
            timeout: Delay::new(Instant::now() + client.timeout_duration),

            sender: client.sender.clone(),
            received: client.received.clone(),
            subscriptions: client.subscriptions.clone(),
            client_state: client.state.clone(),
        }
    }
}
impl<T: Transport> Future for SubscriptionFuture<T> {
    type Item = Id<RouterScope>;
    type Error = Error;

    fn poll(&mut self) -> Result<Async<Self::Item>, Self::Error> {
        loop {
            trace!("SubscriptionFuture: {:?}", self.state);
            super::check_for_timeout(&mut self.timeout)?;

            match *self.client_state.read() {
                ClientState::Established => {}
                ref state => {
                    error!(
                        "SubscriptionFuture with unexpected client state {:?}",
                        state
                    );
                    return Err(WampError::InvalidClientState.into());
                }
            }

            let mut pending = false;
            self.state = match self.state {
                // Step 1: Add the subscription request to the sender's message queue. If the queue
                // is full, return NotReady.
                SubscriptionFutureState::StartSendSubscribe(ref mut message) => {
                    let message = message.take().expect("invalid SubscriptionFutureState");
                    match self.sender.lock().start_send(message)? {
                        AsyncSink::NotReady(message) => {
                            pending = true;
                            SubscriptionFutureState::StartSendSubscribe(Some(message))
                        }
                        AsyncSink::Ready => SubscriptionFutureState::SendSubscribe,
                    }
                }

                // Step 2: Wait for the sender's message queue to empty. If it's not empty, return
                // NotReady.
                SubscriptionFutureState::SendSubscribe => {
                    match self.sender.lock().poll_complete()? {
                        Async::NotReady => {
                            pending = true;
                            SubscriptionFutureState::SendSubscribe
                        }
                        Async::Ready(_) => SubscriptionFutureState::WaitSubscribed,
                    }
                }

                // Step 3: Wait for a rx::Welcome from the receiver. If we haven't yet received
                // one, return NotReady. Through the magic of PollableSet, the current
                // task will be notified when a new Subscribed message arrives.
                SubscriptionFutureState::WaitSubscribed => match self
                    .received
                    .subscribed
                    .lock()
                    .poll_take(|msg| msg.request == self.request_id)
                {
                    Async::NotReady => {
                        pending = true;
                        SubscriptionFutureState::WaitSubscribed
                    }
                    Async::Ready(msg) => {
                        let (sender, receiver) = oneshot::channel();

                        let listener = SubscriptionListener {
                            values: self.received.clone(),
                            stop_receiver: receiver,
                            subscription_id: msg.subscription,
                            handler: self.handler.take().unwrap(),
                        };
                        tokio::spawn(listener);

                        info!(
                            "Subscribed to {:?} (ID: {:?})",
                            self.topic, msg.subscription
                        );
                        self.subscriptions.lock().insert(
                            msg.subscription,
                            Subscription {
                                subscription_id: msg.subscription,
                                listener_stop_sender: sender,
                            },
                        );

                        return Ok(Async::Ready(msg.subscription));
                    }
                },
            };

            if pending {
                return Ok(Async::NotReady);
            }
        }
    }
}
