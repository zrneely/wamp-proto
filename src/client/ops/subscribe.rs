use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures::{
    future::{poll_fn, select, FutureExt},
    pin_mut, select,
};
use tokio::prelude::*;
use tokio::sync::oneshot;

use crate::{
    client::{watch_for_client_state_change, Broadcast, Client, ClientTaskTracker},
    error::WampError,
    pollable::PollableSet,
    proto::TxMessage,
    transport::Transport,
    Id, MessageBuffer, RouterScope, SessionScope, SubscriptionStream, Uri,
};

async fn subscribe_impl<T: Transport>(
    task_tracker: Arc<ClientTaskTracker<T>>,
    topic: Uri,
    received: Arc<MessageBuffer>,
) -> Result<impl SubscriptionStream, WampError> {
    let request_id = Id::<SessionScope>::next();

    {
        let sender = task_tracker.lock_sender().await;
        poll_fn(|cx| Pin::new(&mut sender).poll_ready(cx))
            .await
            .map_err(|error| WampError::WaitForReadyToSendFailed {
                message_type: "SUBSCRIBE",
                error,
            })?;
        Pin::new(&mut sender)
            .start_send(TxMessage::Subscribe {
                topic: topic.clone(),
                request: request_id,
                options: HashMap::default(),
            })
            .map_err(|error| WampError::MessageSendFailed {
                message_type: "SUBSCRIBE",
                error,
            })?;
    }

    {
        let sender = task_tracker.lock_sender().await;
        poll_fn(|cx| Pin::new(&mut sender).poll_flush(cx))
            .await
            .map_err(|error| WampError::SinkFlushFailed {
                message_type: "SUBSCRIBE",
                error,
            })?;
    }

    // Wait for a SUBSCRIBED or ERROR message.
    let subscribed_msg = poll_fn(|cx| {
        received
            .subscribed
            .poll_take(cx, |msg| msg.request == request_id)
    })
    .fuse();
    let error_msg = poll_fn(|cx| {
        received
            .errors
            .subscribe
            .poll_take(cx, |msg| msg.request == request_id)
    })
    .fuse();

    pin_mut!(subscribed_msg, error_msg);
    select! {
        msg = subscribed_msg => {
            info!("Subscribed to {:?} (ID: {:?})", topic, msg.subscription);
            let (sender, receiver) = oneshot::channel();

            let subscription_stream = SubscriptionStreamImpl {
                values: received.clone(),
                stop_receiver: receiver,
                subscription_id: msg.subscription,
            };

            // Create an EVENT queue for our subscription, and allow
            // the client to stop the stream.
            received
                .event
                .write()
                .insert(msg.subscription, PollableSet::default());
            task_tracker.track_subscription(msg.subscription, sender);

            Ok(subscription_stream)
        }
        msg = error_msg => {
            Err(WampError::ErrorReceived {
                error: msg.error,
                request_type: msg.request_type,
                request_id: msg.request,
            })
        }
    }
}

pub(in crate::client) async fn subscribe<T: Transport>(
    client: &Client<T>,
    topic: Uri,
) -> Result<impl SubscriptionStream, WampError> {
    let wfcsc =
        watch_for_client_state_change(client.state.clone(), |state| state.is_established()).fuse();
    let si = subscribe_impl(client.task_tracker.clone(), topic, client.received.clone()).fuse();

    pin_mut!(wfcsc, si);
    select(wfcsc, si).await.factor_first().0
}

struct SubscriptionStreamImpl {
    values: Arc<MessageBuffer>,
    stop_receiver: oneshot::Receiver<()>,
    subscription_id: Id<RouterScope>,
}
impl SubscriptionStream for SubscriptionStreamImpl {
    fn get_subscription_id(&self) -> Id<RouterScope> {
        self.subscription_id
    }
}
impl Stream for SubscriptionStreamImpl {
    type Item = Broadcast;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Broadcast>> {
        trace!("SubscriptionStream wakeup");

        match self.stop_receiver.poll_unpin(cx) {
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

        match self
            .values
            .event
            .read()
            .get(&self.subscription_id)
            .map(|queue| queue.poll_take_any(cx))
        {
            None => {
                error!(
                    "EVENT queue map is missing entry for subscription ID {:?}",
                    self.subscription_id
                );
                Poll::Ready(None)
            }
            Some(Poll::Pending) => Poll::Pending,
            Some(Poll::Ready(message)) => Poll::Ready(Some(Broadcast {
                arguments: message.arguments.unwrap_or_default(),
                arguments_kw: message.arguments_kw.unwrap_or_default(),
            })),
        }
    }
}
