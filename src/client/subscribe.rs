
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use failure::Error;
use futures::{Async, AsyncSink, Future};
use parking_lot::Mutex;
use tokio::timer::Delay;

use ::{Broadcast, Client, GlobalScope, Id, ReceivedValues, RouterScope, Transport, Uri};
use proto::TxMessage;

/// The result of subscribing to a channel. Can be used to unsubscribe.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct Subscription {
    // The ID of the subscription, chosen by the router.
    subscription_id: Id<RouterScope>,
    // The topic subscribed to.
    topic: Uri,
}

pub(crate) type BroadcastHandler =
    Box<FnMut(Broadcast) -> Box<Future<Item = (), Error = Error>>>;

enum SubscriptionFutureState {
    StartSendSubscribe(Option<TxMessage>),
    SendSubscribe,
    WaitSubscribed,
}

/// A future representing a completed subscription.
pub struct SubscriptionFuture<T: Transport> {
    state: SubscriptionFutureState,

    topic: Uri,
    handler: Option<BroadcastHandler>,
    request_id: Id<GlobalScope>,
    timeout: Delay,

    subscriptions: Arc<Mutex<HashMap<Subscription, BroadcastHandler>>>,

    sender: Arc<Mutex<T>>,
    received: ReceivedValues,
}
impl <T: Transport> SubscriptionFuture<T> {
    pub(crate) fn new(client: &mut Client<T>, topic: Uri, handler: BroadcastHandler) -> Self {
        let request_id = Id::<GlobalScope>::generate();
        SubscriptionFuture {
            state: SubscriptionFutureState::StartSendSubscribe(Some(TxMessage::Subscribe {
                topic: topic.clone(),
                request: request_id,
                options: HashMap::new(),
            })),

            topic, request_id,
            handler: Some(handler),
            timeout: Delay::new(Instant::now() + client.timeout_duration),

            sender: client.sender.clone(),
            received: client.received.clone(),
            subscriptions: client.subscriptions.clone(),
        }
    }
}
impl <T: Transport> Future for SubscriptionFuture<T> {
    type Item = Subscription;
    type Error = Error;

    fn poll(&mut self) -> Result<Async<Self::Item>, Self::Error> {
        loop {
            // Before running the state machine, check for timeout or errors in the protocol.
            super::check_for_timeout_or_error(&mut self.timeout, &mut self.received)?;

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
                SubscriptionFutureState::WaitSubscribed => {
                    match self.received.subscribed.lock().poll_take(
                        |msg| msg.request == self.request_id
                    ) {
                        Async::NotReady => {
                            pending = true;
                            SubscriptionFutureState::WaitSubscribed
                        }
                        Async::Ready(msg) => {
                            let subscription = Subscription {
                                subscription_id: msg.subscription,
                                topic: self.topic.clone(),
                            };

                            self.subscriptions.lock().insert(
                                subscription.clone(),
                                self.handler.take().unwrap(),
                            );

                            return Ok(Async::Ready(subscription));
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
