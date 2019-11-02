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

async fn unsubscribe_impl<T: Transport, S: SubscriptionStream>(
    task_tracker: Arc<ClientTaskTracker<T>>,
    subscription: S,
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
    // After this await, there should be no new EVENTs.
    poll_fn(|cx| {
        received
            .unsubscribed
            .poll_take(cx, |msg| msg.request == request_id)
    })
    .await;

    // Drain any unprocessed messages in the EVENT queue that would have been
    // handled by this subscription to avoid leaking memory.
    received
        .event
        .drain(|msg| msg.subscription == subscription.get_subscription_id());
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
