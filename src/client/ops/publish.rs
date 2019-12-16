use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use futures::{
    future::{poll_fn, select},
    pin_mut, select,
};
use tokio::prelude::*;

use crate::{
    client::{watch_for_client_state_change, Broadcast, Client, ClientTaskTracker},
    error::WampError,
    proto::TxMessage,
    transport::{Transport, TransportableValue as TV},
    uri::Uri,
    GlobalScope, Id, MessageBuffer, SessionScope,
};

async fn publish_impl<T: Transport, I: IntoIterator<Item = Broadcast>>(
    task_tracker: Arc<ClientTaskTracker<T>>,
    topic: &Uri,
    messages: I,
) -> Result<(), WampError> {
    {
        let mut sender = task_tracker.get_sender().lock().await;
        for message in messages {
            poll_fn(|cx| Pin::new(&mut *sender).poll_ready(cx))
                .await
                .map_err(|error| WampError::WaitForReadyToSendFailed {
                    message_type: "PUBLISH",
                    error,
                })?;
            Pin::new(&mut *sender)
                .start_send(TxMessage::Publish {
                    request: Id::<SessionScope>::next(),
                    options: HashMap::new(),
                    topic: topic.clone(),
                    arguments: Some(message.arguments),
                    arguments_kw: Some(message.arguments_kw),
                })
                .map_err(|error| WampError::MessageSendFailed {
                    message_type: "PUBLISH",
                    error,
                })?;
        }
    }

    {
        let mut sender = task_tracker.get_sender().lock().await;
        poll_fn(|cx| Pin::new(&mut *sender).poll_flush(cx))
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

async fn publish_with_ack_impl<T: Transport>(
    task_tracker: Arc<ClientTaskTracker<T>>,
    received: Arc<MessageBuffer>,
    topic: &Uri,
    message: Broadcast,
) -> Result<Id<GlobalScope>, WampError> {
    let request_id = Id::<SessionScope>::next();

    {
        let mut sender = task_tracker.get_sender().lock().await;
        poll_fn(|cx| Pin::new(&mut *sender).poll_ready(cx))
            .await
            .map_err(|error| WampError::WaitForReadyToSendFailed {
                message_type: "PUBLISH",
                error,
            })?;
        Pin::new(&mut *sender)
            .start_send(TxMessage::Publish {
                request: request_id,
                options: {
                    let mut options = HashMap::new();
                    options.insert("acknowledge".into(), TV::Bool(true));
                    options
                },
                topic: topic.clone(),
                arguments: Some(message.arguments),
                arguments_kw: Some(message.arguments_kw),
            })
            .map_err(|error| WampError::MessageSendFailed {
                message_type: "PUBLISH",
                error,
            })?;
    }

    {
        let mut sender = task_tracker.get_sender().lock().await;
        poll_fn(|cx| Pin::new(&mut *sender).poll_flush(cx))
            .await
            .map_err(|error| WampError::SinkFlushFailed {
                message_type: "PUBLISH",
                error,
            })?;
    }

    let published_msg = poll_fn(|cx| {
        received
            .published
            .poll_take(cx, |msg| msg.request == request_id)
    })
    .fuse();
    let published_err = poll_fn(|cx| {
        received
            .errors
            .publish
            .poll_take(cx, |msg| msg.request == request_id)
    })
    .fuse();

    pin_mut!(published_msg, published_err);
    select! {
        msg = published_msg => {
            Ok(msg.publication)
        },
        msg = published_err => {
            Err(WampError::ErrorReceived {
                error: msg.error,
                request_type: msg.request_type,
                request_id: msg.request,
            })
        }
    }
}

pub(in crate::client) async fn publish<T: Transport, I: IntoIterator<Item = Broadcast>>(
    client: &Client<T>,
    topic: &Uri,
    message: I,
) -> Result<(), WampError> {
    let wfcsc =
        watch_for_client_state_change(client.state.clone(), |state| state.is_established()).fuse();
    let pi = publish_impl(client.task_tracker.clone(), topic, message).fuse();

    pin_mut!(wfcsc, pi);
    select(wfcsc, pi).await.factor_first().0
}

pub(in crate::client) async fn publish_with_ack<T: Transport>(
    client: &Client<T>,
    topic: &Uri,
    message: Broadcast,
) -> Result<Id<GlobalScope>, WampError> {
    let wfcsc =
        watch_for_client_state_change(client.state.clone(), |state| state.is_established()).fuse();
    let pi = publish_with_ack_impl(
        client.task_tracker.clone(),
        client.received.clone(),
        topic,
        message,
    )
    .fuse();

    pin_mut!(wfcsc, pi);
    select(wfcsc, pi).await.factor_first().0
}
