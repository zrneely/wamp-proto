use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use futures::{
    future::{poll_fn, select},
    pin_mut,
};
use tokio::prelude::*;

use crate::{
    client::{watch_for_client_state_change, Client, ClientTaskTracker},
    error::WampError,
    proto::TxMessage,
    transport::Transport,
    Broadcast, Id, SessionScope, Uri,
};

async fn publish_impl<T: Transport>(
    task_tracker: Arc<ClientTaskTracker<T>>,
    topic: Uri,
    message: Broadcast,
) -> Result<(), WampError> {
    {
        let sender = task_tracker.lock_sender().await;
        poll_fn(|cx| Pin::new(&mut sender).poll_ready(cx))
            .await
            .map_err(|error| WampError::WaitForReadyToSendFailed {
                message_type: "PUBLISH",
                error,
            })?;
        Pin::new(&mut sender)
            .start_send(TxMessage::Publish {
                request: Id::<SessionScope>::next(),
                options: HashMap::new(),
                topic,
                arguments: Some(message.arguments),
                arguments_kw: Some(message.arguments_kw),
            })
            .map_err(|error| WampError::MessageSendFailed {
                message_type: "PUBLISH",
                error,
            })?;
    }

    {
        let mut sender = task_tracker.lock_sender().await;
        poll_fn(|cx| Pin::new(&mut sender).poll_flush(cx))
            .await
            .map_err(|error| WampError::SinkFlushFailed {
                message_type: "PUBLISH",
                error,
            })?;
    }

    // Note: since we're not requesting acknowledgement, the broker
    // will not tell us if the message request fails.

    Ok(())
}

pub(in crate::client) async fn publish<T: Transport>(
    client: &Client<T>,
    topic: Uri,
    message: Broadcast,
) -> Result<(), WampError> {
    let wfcsc =
        watch_for_client_state_change(client.state.clone(), |state| state.is_established()).fuse();
    let pi = publish_impl(client.task_tracker.clone(), topic, message).fuse();

    pin_mut!(wfcsc, pi);
    select(wfcsc, pi).await.factor_first().0
}
