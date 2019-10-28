use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use failure::Error;

use crate::{
    client::{Client, ClientState, ClientTaskTracker},
    error::WampError,
    pollable::PollableValue,
    proto::TxMessage,
    transport::Transport,
    Broadcast, Id, SessionScope, Uri,
};

pub(in crate::client) fn publish<T: Transport>(
    client: &Client<T>,
    topic: Uri,
    message: Broadcast,
) -> impl Future<Output = Result<(), Error>> {
    PublishFuture {
        state: PublishFutureState::PrepareSendPublish,
        message: TxMessage::Publish {
            request: Id::<SessionScope>::next(),
            options: HashMap::new(),
            topic,
            arguments: Some(message.arguments),
            arguments_kw: Some(message.arguments_kw),
        },
        client_state: client.state.clone(),
        task_tracker: client.task_tracker.clone(),
    }
}

#[derive(Debug)]
enum PublishFutureState {
    PrepareSendPublish,
    SendAndFlushPublish,
}

/// A future used to drive publishing a message
struct PublishFuture<T: Transport> {
    state: PublishFutureState,
    message: Option<TxMessage>,

    client_state: PollableValue<ClientState>,
    task_tracker: Arc<ClientTaskTracker<T>>,
}
impl<T: Transport> Future for PublishFuture<T> {
    type Output = Result<(), Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Error>> {
        loop {
            trace!("PublishFuture: {:?}", self.state);

            match self.client_state.read(Some(cx)) {
                ClientState::Established => {}
                state => {
                    warn!("PublishFuture with unexpected client state {:?}", state);
                    return Poll::Ready(Err(WampError::InvalidClientState.into()));
                }
            }

            let mut pending = false;
            self.state = match self.state {
                PublishFutureState::PrepareSendPublish => {
                    match self.task_tracker.get_sender().lock().poll_ready(cx) {
                        Poll::Pending => {
                            pending = true;
                            PublishFutureState::PrepareSendPublish
                        }
                        Poll::Ready(Ok(())) => PublishFutureState::SendAndFlushPublish,
                        Poll::Ready(Err(error)) => {
                            error!("Failed to prepare to send PUBLISH: {}", error);
                            return Poll::Ready(Err(error));
                        }
                    }
                }

                PublishFutureState::SendAndFlushPublish => {
                    if let Some(message) = self.message.take() {
                        self.task_tracker.get_sender().lock().start_send(message)?;
                    }

                    match self.task_tracker.get_sender().lock().poll_flush(cx)? {
                        Poll::Pending => {
                            pending = true;
                            PublishFutureState::SendAndFlushPublish
                        }
                        Poll::Ready(Ok(())) => {
                            info!("Sent PUBLISH successfully");
                            return Poll::Ready(Ok(()));
                        }
                        Poll::Ready(Err(error)) => {
                            error!("Failed to send PUBLISH");
                            return Poll::Ready(Err(error));
                        }
                    }
                }
            };

            if pending {
                return Poll::Pending;
            }
        }
    }
}
