use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use failure::Error;
use futures::{sync::oneshot, Async, AsyncSink, Future};
use tokio::timer::Delay;

use client::{Broadcast, Client, ClientState, ClientTaskTracker};
use error::WampError;
use pollable::PollableValue;
use proto::TxMessage;
use {Id, ReceivedValues, RouterScope, SessionScope, Transport, Uri};

// The future (task) that listens for new events published to a channel we're
// subscribed to.
struct SubscriptionListener<F, R>
where
    F: Fn(Broadcast) -> R + Send + 'static,
    R: Future<Item = (), Error = Error> + Send + 'static,
{
    values: ReceivedValues,
    stop_receiver: oneshot::Receiver<()>,
    subscription_id: Id<RouterScope>,
    handler: F,
}
impl<F, R> Future for SubscriptionListener<F, R>
where
    F: Fn(Broadcast) -> R + Send + 'static,
    R: Future<Item = (), Error = Error> + Send + 'static,
{
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
                    info!(
                        "Subscription {:?} received event: {:?}",
                        self.subscription_id, event
                    );

                    // Spawn the event handler on its own task. We can't do it as a part of this function,
                    // or handler-produced futures will stop executing if the subscription is cancelled.
                    tokio::spawn(
                        (self.handler)(Broadcast {
                            arguments: event.arguments.unwrap_or_else(Vec::new),
                            arguments_kw: event.arguments_kw.unwrap_or_else(HashMap::new),
                        }).map_err(|err| {
                            warn!("Event handler produced error: {:?}", err);
                            ()
                        }),
                    );
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
pub(in client) struct SubscriptionFuture<T, F, R>
where
    T: Transport,
    F: Fn(Broadcast) -> R + Send + 'static,
    R: Future<Item = (), Error = Error> + Send + 'static,
{
    state: SubscriptionFutureState,

    topic: Uri,
    handler: Option<F>,
    request_id: Id<SessionScope>,
    timeout: Delay,

    received: ReceivedValues,
    client_state: PollableValue<ClientState>,
    task_tracker: Arc<ClientTaskTracker<T>>,
}
impl<T, F, R> SubscriptionFuture<T, F, R>
where
    T: Transport,
    F: Fn(Broadcast) -> R + Send + 'static,
    R: Future<Item = (), Error = Error> + Send + 'static,
{
    pub fn new(client: &Client<T>, topic: Uri, handler: F) -> Self {
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

            received: client.received.clone(),
            client_state: client.state.clone(),
            task_tracker: client.task_tracker.clone(),
        }
    }
}
impl<T, F, R> Future for SubscriptionFuture<T, F, R>
where
    T: Transport,
    F: Fn(Broadcast) -> R + Send + 'static,
    R: Future<Item = (), Error = Error> + Send + 'static,
{
    type Item = Id<RouterScope>;
    type Error = Error;

    fn poll(&mut self) -> Result<Async<Self::Item>, Self::Error> {
        loop {
            trace!("SubscriptionFuture: {:?}", self.state);
            ::client::check_for_timeout(&mut self.timeout)?;

            match self.client_state.read(true) {
                ClientState::Established => {}
                ref state => {
                    warn!(
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
                    match self.task_tracker.get_sender().lock().start_send(message)? {
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
                    match self.task_tracker.get_sender().lock().poll_complete()? {
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
                        self.task_tracker
                            .track_subscription(msg.subscription, sender);
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
