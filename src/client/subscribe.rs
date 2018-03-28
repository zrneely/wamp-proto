
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use failure::Error;
use futures::{Async, Future, Sink};
use futures::task::Context;
use parking_lot::Mutex;
use tokio_timer::Sleep;

use ::{Broadcast, Client, GlobalScope, Id, RouterScope, Transport, TsPollSet, Uri};
use client::TIMER;
use error::WampError;
use proto::{rx, TxMessage};

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
    timeout: Sleep,
    timeout_duration: Duration,

    subscriptions: Arc<Mutex<HashMap<Subscription, BroadcastHandler>>>,

    sender: Arc<Mutex<T>>,
    received: TsPollSet<rx::Subscribed>,
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
            timeout: TIMER.sleep(client.timeout_duration),
            timeout_duration: client.timeout_duration,

            sender: client.sender.clone(),
            subscribeds: client.incoming_values.subscribeds.clone(),
            subscriptions: client.subscriptions.clone(),
        }
    }
}
impl <T: Transport> Future for SubscriptionFuture<T> {
    type Item = Subscription;
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
                // Step 1: Add the subscription request to the sender's message queue. If the queue
                // is full, return NotReady.
                SubscriptionFutureState::StartSendSubscribe(Some(message)) => {
                    match self.sender.lock().poll_ready(cx)? {
                        Async::Pending => {
                            self.state = SubscriptionFutureState::StartSendSubscribe(
                                Some(message)
                            );
                            return Ok(Async::Pending);
                        }
                        Async::Ready(_) => {
                            self.state = SubscriptionFutureState::SendSubscribe;
                            sender.start_send(message)?;
                        }
                    }
                }
                SubscriptionFutureState::StartSendSubscribe(None) => {
                    panic!("invalid SubscriptionFutureState");
                }

                // Step 2: Wait for the sender's message queue to empty. If it's not empty, return
                // NotReady.
                SubscriptionFutureState::SendSubscribe => {
                    try_ready!(self.sender.lock().poll_flush(cx));
                    self.state = SubscriptionFutureState::WaitSubscribed;
                }

                // Step 3: Wait for a rx::Welcome from the receiver. If we haven't yet received
                // one, return NotReady. Through the magic of PollableSet, the current
                // task will be notified when a new Subscribed message arrives.
                SubscriptionFutureState::WaitSubscribed => {
                    let msg = try_ready!(self.received.lock().poll_take(
                        |msg| msg.request == self.request_id,
                        cx,
                    ));

                    let subscription = Subscription {
                        subscription_id: msg.subscription,
                        topic: self.topic.clone(),
                    };

                    self.subscriptions.lock().insert(
                        subscription.clone(),
                        self.handler.take().expect("SubscriptionFuture missing handler!"),
                    );

                    return Ok(Async::Ready(subscription));
                }
            }
        }
    }
}
