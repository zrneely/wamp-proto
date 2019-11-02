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
    client::{watch_for_client_state_change, Client, ClientState, ClientTaskTracker},
    proto::TxMessage,
    transport::Transport,
    MessageBuffer, Uri,
};

async fn close_impl<T: Transport>(
    task_tracker: Arc<ClientTaskTracker<T>>,
    reason: Uri,
    received: Arc<MessageBuffer>,
) -> Result<(), Error> {
    {
        let sender = task_tracker.lock_sender().await;
        poll_fn(|cx| Pin::new(&mut sender).poll_ready(cx)).await?;
        Pin::new(&mut sender).start_send(TxMessage::Goodbye {
            details: HashMap::default(),
            reason,
        })?;
    }

    {
        let sender = task_tracker.lock_sender().await;
        poll_fn(|cx| Pin::new(&mut sender).poll_flush(cx)).await?;
    }

    // Wait for a goodbye message to come in, confirming the disconnect.
    poll_fn(|cx| received.goodbye.poll_take_any(cx)).await;

    Ok(())
}

/// Asynchronous function to cleanly close a connection.
pub(in crate::client) async fn close<T: Transport>(
    client: &Client<T>,
    reason: Uri,
) -> Result<(), Error> {
    let wfcsc = watch_for_client_state_change(client.state.clone(), |state| {
        state == ClientState::ShuttingDown || state == ClientState::Closed
    })
    .fuse();
    let ci = close_impl(client.task_tracker.clone(), reason, client.received.clone()).fuse();

    pin_mut!(wfcsc, ci);
    select(wfcsc, ci).await.factor_first().0
}
