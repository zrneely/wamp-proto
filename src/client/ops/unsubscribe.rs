use std::pin::Pin;
use std::sync::Arc;

use failure::Error;
use futures::{
    future::{poll_fn, select},
    pin_mut,
};
use tokio::prelude::*;

use crate::{
    client::{watch_for_client_state_change, Client, ClientTaskTracker},
    proto::TxMessage,
    transport::Transport,
    Id, MessageBuffer, SessionScope, SubscriptionStream,
};

async fn unsubscribe_impl<T: Transport>(
    task_tracker: Arc<ClientTaskTracker<T>>,
    subscription: impl SubscriptionStream,
    received: Arc<MessageBuffer>,
) -> Result<(), Error> {
    let request_id = Id::<SessionScope>::next();

    {
        let sender = task_tracker.lock_sender().await;
        poll_fn(|cx| Pin::new(&mut sender).poll_ready(cx)).await?;
        Pin::new(&mut sender).start_send(TxMessage::Unsubscribe {
            request: request_id,
            subscription: subscription.get_subscription_id(),
        })?;
    }

    {
        let sender = task_tracker.lock_sender().await;
        poll_fn(|cx| Pin::new(&mut sender).poll_flush(cx)).await?;
    }

    // Wait for an UNSUBSCRIBED message to come in, confirming the unsubscription.
    // After this await, there should be no new EVENTs, so it's safe to clean up
    // the EVENT queue.
    poll_fn(|cx| {
        received
            .unsubscribed
            .poll_take(cx, |msg| msg.request == request_id)
    })
    .await;

    // Clean up the event queue and SubscriptionStream.
    received
        .event
        .write()
        .remove(&subscription.get_subscription_id());
    task_tracker.stop_subscription(subscription.get_subscription_id());

    Ok(())
}

pub(in crate::client) async fn unsubscribe<T: Transport, S: SubscriptionStream>(
    client: &Client<T>,
    subscription: S,
) -> Result<(), Error> {
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
