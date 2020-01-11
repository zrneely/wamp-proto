use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use futures::{
    future::{poll_fn, select},
    pin_mut, FutureExt, Sink,
};

use crate::{
    client::{watch_for_client_state_change, Client, ClientState, ClientTaskTracker},
    error::WampError,
    proto::TxMessage,
    transport::Transport,
    uri::Uri,
    MessageBuffer,
};

async fn close_impl<T: Transport>(
    task_tracker: Arc<ClientTaskTracker<T>>,
    reason: Uri,
    received: Arc<MessageBuffer>,
) -> Result<(), WampError> {
    {
        let mut sender = task_tracker.get_sender().lock().await;
        poll_fn(|cx| Pin::new(&mut *sender).poll_ready(cx))
            .await
            .map_err(|error| WampError::WaitForReadyToSendFailed {
                message_type: "GOODBYE",
                error,
            })?;
        Pin::new(&mut *sender)
            .start_send(TxMessage::Goodbye {
                details: HashMap::default(),
                reason,
            })
            .map_err(|error| WampError::MessageSendFailed {
                message_type: "GOODBYE",
                error,
            })?;
    }

    {
        let mut sender = task_tracker.get_sender().lock().await;
        poll_fn(|cx| Pin::new(&mut *sender).poll_flush(cx))
            .await
            .map_err(|error| WampError::SinkFlushFailed {
                message_type: "GOODBYE (flush)",
                error,
            })?;
    }

    // Wait for a goodbye message to come in, confirming the disconnect.
    poll_fn(|cx| received.goodbye.poll_take_any(cx)).await;

    Ok(())
}

/// Asynchronous function to cleanly close a connection.
pub(in crate::client) async fn close<T: Transport>(
    client: &Client<T>,
    reason: Uri,
) -> Result<(), WampError> {
    let wfcsc = watch_for_client_state_change(client.state.clone(), |state| {
        state == ClientState::ShuttingDown || state == ClientState::Closed
    })
    .fuse();
    let ci = close_impl(client.task_tracker.clone(), reason, client.received.clone()).fuse();

    pin_mut!(wfcsc, ci);
    select(wfcsc, ci).await.factor_first().0
}
