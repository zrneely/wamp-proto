use std::pin::Pin;
use std::sync::Arc;

use futures::{
    future::{poll_fn, select},
    pin_mut, select,
};
use tokio::prelude::*;

use crate::{
    client::{watch_for_client_state_change, Client, ClientTaskTracker},
    error::WampError,
    proto::TxMessage,
    transport::Transport,
    Id, MessageBuffer, SessionScope, SubscriptionStream,
};

async fn unsubscribe_impl<T: Transport>(
    task_tracker: Arc<ClientTaskTracker<T>>,
    subscription: impl SubscriptionStream,
    received: Arc<MessageBuffer>,
) -> Result<(), WampError> {
    let request_id = Id::<SessionScope>::next();

    {
        let sender = task_tracker.lock_sender().await;
        poll_fn(|cx| Pin::new(&mut sender).poll_ready(cx))
            .await
            .map_err(|error| WampError::WaitForReadyToSendFailed {
                message_type: "UNSUBSCRIBE",
                error,
            })?;
        Pin::new(&mut sender)
            .start_send(TxMessage::Unsubscribe {
                request: request_id,
                subscription: subscription.get_subscription_id(),
            })
            .map_err(|error| WampError::MessageSendFailed {
                message_type: "UNSUBSCRIBE",
                error,
            })?;
    }

    {
        let sender = task_tracker.lock_sender().await;
        poll_fn(|cx| Pin::new(&mut sender).poll_flush(cx))
            .await
            .map_err(|error| WampError::SinkFlushFailed {
                message_type: "UNSUBSCRIBE",
                error,
            })?;
    }

    // Wait for an UNSUBSCRIBED message to come in, confirming the unsubscription.
    // After this await, there should be no new EVENTs, so it's safe to clean up
    // the EVENT queue.
    let unsubscribed_msg = poll_fn(|cx| {
        received
            .unsubscribed
            .poll_take(cx, |msg| msg.request == request_id)
    })
    .fuse();
    let error_msg = poll_fn(|cx| {
        received
            .errors
            .unsubscribe
            .poll_take(cx, |msg| msg.request == request_id)
    })
    .fuse();

    pin_mut!(unsubscribed_msg, error_msg);
    select! {
        msg = unsubscribed_msg => {
            // Clean up the event queue and SubscriptionStream.
            received
                .event
                .write()
                .remove(&subscription.get_subscription_id());
            task_tracker.stop_subscription(subscription.get_subscription_id());

            Ok(())
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

pub(in crate::client) async fn unsubscribe<T: Transport, S: SubscriptionStream>(
    client: &Client<T>,
    subscription: S,
) -> Result<(), WampError> {
    let wfcsc =
        watch_for_client_state_change(client.state.clone(), |state| state.is_established()).fuse();
    let ui = unsubscribe_impl(
        client.task_tracker.clone(),
        subscription,
        client.received.clone(),
    )
    .fuse();

    pin_mut!(wfcsc, ui);
    select(wfcsc, ui).await.factor_first().0
}
