use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use failure::Error;
use tokio::prelude::*;
use tokio::sync::oneshot;

use crate::{
    client::{Broadcast, Client, ClientState, ClientTaskTracker},
    error::WampError,
    pollable::PollableValue,
    proto::TxMessage,
    transport::Transport,
    Id, MessageBuffer, RouterScope, SessionScope, Uri,
};

pub(in crate::client) fn subscribe<T: Transport>(
    client: &Client<T>,
    topic: Uri,
) -> impl Future<Item = Result<impl Stream<Broadcast>, Error>> {
    let request_id = Id::<SessionScope>::next();
    SubscriptionFuture {
        state: SubscriptionFutureState::PrepareSendSubscribe,
        message: Some(TxMessage::Subscribe {
            topic: topic.clone(),
            request: request_id,
            options: HashMap::new(),
        }),

        topic,
        request_id,

        received: client.received.clone(),
        client_state: client.state.clone(),
        task_tracker: client.task_tracker.clone(),
    }
}

pub(in crate::client) struct SubscriptionStream<T: Transport> {
    values: MessageBuffer,
    stop_receiver: oneshot::Receiver<()>,
    subscription_id: Id<RouterScope>,
}
impl<T: Transport> Stream<Broadcast> for SubscriptionStream<T> {
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Broadcast>> {
        trace!("SubscriptionStream wakeup");

        loop {
            match self.stop_receiver.poll(cx) {
                Poll::Pending => {}
                // Either we've been told to stop or the sender was dropped,
                // in which case we should stop anyway.
                Poll::Ready(Ok(())) => {
                    debug!("SubscriptionStream told to stop!");
                    return Poll::Ready(None);
                }
                Poll::Ready(Err(err)) => {
                    error!("SubscriptionStream stop listener broken: {}", err);
                    return Poll::Ready(None);
                }
            }

            // TODO: have a different event queue for each subscription so we don't need
            // to pass a lambda here.
            match self
                .values
                .event
                .poll_take(cx, |evt| evt.subscription == self.subscription_id)
            {
                Poll::Pending => {
                    return Poll::Pending;
                }
                Poll::Ready(message) => {
                    return Poll::Ready(Some(Broadcast {
                        arguments: message.arguments.unwrap_or_else(Vec::new),
                        arguments_kw: message.arguments_kw.unwrap_or_else(HashMap::new),
                    }));
                }
            }
        }
    }
}
impl<T: Transport> Drop for SubscriptionStream {
    fn drop(&mut self) {
        // TODO: unsubscribe
        unimplemented!()
    }
}

#[derive(Debug)]
enum SubscriptionFutureState {
    PrepareSendSubscribe,
    SendAndFlushSubscribe,
    WaitSubscribed,
}

/// A future representing a completed subscription.
struct SubscriptionFuture<T: Transport> {
    state: SubscriptionFutureState,
    message: Option<TxMessage>,

    topic: Uri,
    request_id: Id<SessionScope>,

    received: MessageBuffer,
    client_state: PollableValue<ClientState>,
    task_tracker: Arc<ClientTaskTracker<T>>,
}
impl<T: Transport> Future for SubscriptionFuture<T> {
    type Output = Result<SubscriptionStream, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<SubscriptionStream, Error>> {
        loop {
            trace!("SubscriptionFuture: {:?}", self.state);

            match self.client_state.read(Some(cx)) {
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
                SubscriptionFutureState::PrepareSendSubscribe => {
                    match self.task_tracker.get_sender().lock().poll_ready(cx) {
                        Poll::Pending => {
                            pending = true;
                            SubscriptionFutureState::PrepareSendSubscribe
                        }
                        Poll::Ready(Ok(())) => SubscriptionFutureState::SendAndFlushGoodbye,
                        Poll::Ready(Err(error)) => {
                            error!("Failed to prepare to send SUBSCRIBE: {}", error);
                            return Poll::Ready(Err(error));
                        }
                    }
                }

                SubscriptionFutureState::SendAndFlushSubscribe => {
                    if let Some(message) = self.message.take() {
                        self.task_tracker.get_sender().lock().start_send(message)?;
                    }

                    match self.task_tracker.get_sender().lock().poll_flush(cx) {
                        Poll::Pending => {
                            pending = true;
                            SubscriptionFutureState::SendAndFlushSubscribe
                        }
                        Poll::Ready(Ok(())) => SubscriptionFutureState::WaitSubscribed,
                        Poll::Ready(Err(error)) => {
                            error!("Failed to send SUBSCRIBE: {}", error);
                            return Poll::Ready(Err(error));
                        }
                    }
                }

                // Step 3: Wait for a rx::Welcome from the receiver. If we haven't yet received
                // one, return NotReady. Through the magic of PollableSet, the current
                // task will be notified when a new Subscribed message arrives.
                SubscriptionFutureState::WaitSubscribed => match self
                    .received
                    .subscribed
                    .lock()
                    .poll_take(cx, |msg| msg.request == self.request_id)
                {
                    Poll::Pending => {
                        pending = true;
                        SubscriptionFutureState::WaitSubscribed
                    }
                    Poll::Ready(msg) => {
                        info!(
                            "Subscribed to {:?} (ID: {:?})",
                            self.topic, msg.subscription
                        );
                        let (sender, receiver) = oneshot::channel();

                        let subscription_stream = SubscriptionStream {
                            values: self.received.clone(),
                            stop_receiver: receiver,
                            subscription_id: msg.subscription,
                        };

                        // This is used to allow the client to stop existing subscriptions
                        // when it is closed.
                        self.task_tracker
                            .track_subscription(msg.subscription, sender);
                        return Poll::Ready(Ok(subscription_stream));
                    }
                },
            };

            if pending {
                return Poll::Pending;
            }
        }
    }
}
