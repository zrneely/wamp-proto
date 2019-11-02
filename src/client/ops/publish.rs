use std::collections::HashMap;
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
    Broadcast, Id, SessionScope, Uri,
};

async fn publish_impl<T: Transport>(
    task_tracker: Arc<ClientTaskTracker<T>>,
    topic: Uri,
    message: Broadcast,
) -> Result<(), Error> {
    {
        let sender = task_tracker.lock_sender().await;
        poll_fn(|cx| Pin::new(&mut sender).poll_ready(cx)).await?;
        Pin::new(&mut sender).start_send(TxMessage::Publish {
            request: Id::<SessionScope>::next(),
            options: HashMap::new(),
            topic,
            arguments: Some(message.arguments),
            arguments_kw: Some(message.arguments_kw),
        })?;
    }

    {
        let mut sender = task_tracker.lock_sender().await;
        poll_fn(|cx| Pin::new(&mut sender).poll_flush(cx)).await?;
    }

    Ok(())
}

pub(in crate::client) async fn publish<T: Transport>(
    client: &Client<T>,
    topic: Uri,
    message: Broadcast,
) -> Result<(), Error> {
    let wfcsc =
        watch_for_client_state_change(client.state.clone(), |state| state.is_established()).fuse();
    let pi = publish_impl(client.task_tracker.clone(), topic, message).fuse();

    pin_mut!(wfcsc, pi);
    select(wfcsc, pi).await.factor_first().0
}
