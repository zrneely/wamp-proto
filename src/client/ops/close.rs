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
    MessageBuffer, Uri,
};

/// Asynchronous function to cleanly close a connection.
pub(in crate::client) fn close<T: Transport>(
    client: &Client<T>,
    reason: Uri,
) -> impl Future<Output = Result<(), Error>> {
    CloseFuture {
        state: CloseFutureState::PrepareSendGoodbye,
        message: Some(TxMessage::Goodbye {
            details: HashMap::new(),
            reason,
        }),
        received: client.received.clone(),
        client_state: client.state.clone(),
        task_tracker: client.task_tracker.clone(),
    }
}

#[derive(Debug)]
enum CloseFutureState {
    PrepareSendGoodbye,
    SendAndFlushGoodbye,
    WaitResponse,
}

struct CloseFuture<T: Transport> {
    state: CloseFutureState,
    message: Option<TxMessage>,

    received: MessageBuffer,
    client_state: PollableValue<ClientState>,
    task_tracker: Arc<ClientTaskTracker<T>>,
}
impl<T: Transport> Future for CloseFuture<T> {
    type Output = Result<(), Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Error>> {
        loop {
            trace!("CloseFuture: {:?}", self.state);

            // We DO want to be notified if the client state changes while we're running.
            match self.client_state.read(Some(cx)) {
                ClientState::ShuttingDown | ClientState::Closed => {}
                ref state => {
                    error!("CloseFuture with unexpected client state {:?}", state);
                    return Poll::Ready(Err(WampError::InvalidClientState.into()));
                }
            }

            let mut pending = false;
            self.state = match self.state {
                // Step 1: Wait for the queue to become ready.
                CloseFutureState::PrepareSendGoodbye => {
                    match self.task_tracker.get_sender().lock().poll_ready(cx) {
                        Poll::Pending => {
                            pending = true;
                            CloseFutureState::PrepareSendGoodbye
                        }
                        Poll::Ready(Ok(())) => CloseFutureState::SendAndFlushGoodbye,
                        Poll::Ready(Err(error)) => {
                            error!("Failed to prepare to send goodbye: {}", error);
                            return Poll::Ready(Err(error));
                        }
                    }
                }

                // Step 2: Add the goodbye message to the sender's message queue, and wait for it
                // to be flushed.
                CloseFutureState::SendAndFlushGoodbye => {
                    // If this is our first time getting here, start to send the message.
                    if let Some(message) = self.message.take() {
                        self.task_tracker.get_sender().lock().start_send(message)?;
                    }

                    match self.task_tracker.get_sender().lock().poll_flush(cx) {
                        Poll::Pending => {
                            pending = true;
                            CloseFutureState::SendAndFlushGoodbye
                        }
                        Poll::Ready(Ok(())) => CloseFutureState::WaitResponse,
                        Poll::Ready(Err(error)) => {
                            error!("Failed to send goodbye: {}", error);
                            return Poll::Ready(Err(error));
                        }
                    }
                }

                // Step 3: Wait for the goodbye response from the router.
                CloseFutureState::WaitResponse => {
                    match self.received.goodbye.lock().poll_take(cx, |_| true) {
                        Poll::Pending => {
                            pending = true;
                            CloseFutureState::WaitResponse
                        }
                        Poll::Ready(msg) => {
                            info!(
                                "WAMP session closed: response {:?} ({:?})",
                                msg.details, msg.reason
                            );
                            self.client_state.write(ClientState::Closed);
                            return Poll::Ready(Ok(()));
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
