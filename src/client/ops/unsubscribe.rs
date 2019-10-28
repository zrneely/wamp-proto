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
    Id, MessageBuffer, RouterScope, SessionScope, SubscriptionStream,
};

pub(in crate::client) fn unsubscribe<T: Transport, S: SubscriptionStream>(
    client: &Client<T>,
    subscription: S,
) -> impl Future<Output = Result<(), Error>> {
    let request = Id::<SessionScope>::next();

    UnsubscriptionFuture {
        state: UnsubscribeFutureState::PrepareSendUnsubscribe,
        message: Some(TxMessage::Unsubscribe {
            request,
            subscription: subscription.get_subscription_id(),
        }),

        subscription,
        request: Id::<SessionScope>::next(),

        received: client.received.clone(),
        client_state: client.state.clone(),
        task_tracker: client.task_tracker.clone(),
    }
}

#[derive(Debug)]
enum UnsubscribeFutureState {
    PrepareSendUnsubscribe,
    SendAndFlushUnsubscribe,
    WaitUnsubscribed,
}

/// A future representing a completed unsubscription.
struct UnsubscriptionFuture<T: Transport, S: SubscriptionStream> {
    state: UnsubscribeFutureState,
    message: Option<TxMessage>,

    subscription: S,
    request: Id<SessionScope>,

    received: MessageBuffer,
    client_state: PollableValue<ClientState>,
    task_tracker: Arc<ClientTaskTracker<T>>,
}
impl<T: Transport> UnsubscriptionFuture<T> {
    pub fn new(client: &Client<T>, subscription: Id<RouterScope>) -> Self {
        UnsubscriptionFuture {
            state: UnsubscribeFutureState::StartSendUnsubscribe,
        }
    }
}
impl<T: Transport> Future for UnsubscriptionFuture<T> {
    type Output = Result<(), Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Error>> {
        loop {
            trace!("UnsubscribeFuture: {:?}", self.state);

            match self.client_state.read(Some(cx)) {
                ClientState::Established => {}
                ref state => {
                    warn!("UnsubscribeFuture with unexpected client state {:?}", state);
                    return Poll::Ready(Err(WampError::InvalidClientState.into()));
                }
            }

            let mut pending = false;
            self.state = match self.state {
                UnsubscribeFutureState::PrepareSendUnsubscribe => {
                    match self.task_tracker.get_sender().lock().poll_ready(cx) {
                        Poll::Pending => {
                            pending = true;
                            UnsubscribeFutureState::PrepareSendUnsubscribe
                        }
                        Poll::Ready(Ok(())) => UnsubscribeFutureState::SendAndFlushUnsubscribe,
                        Poll::Ready(Err(error)) => {
                            error!("Failed to prepare to send UNSUBSCRIBE: {}", error);
                            return Poll::Ready(Err(error));
                        }
                    }
                }

                UnsubscribeFutureState::SendAndFlushUnsubscribe => {
                    if let Some(message) = self.message.take() {
                        self.task_tracker.get_sender().lock().start_send(message)?;
                    }

                    match self.task_tracker.get_sender().lock().poll_flush(cx) {
                        Poll::Pending => {
                            pending = true;
                            UnsubscribeFutureState::SendAndFlushUnsubscribe
                        }
                        Poll::Ready(Ok(())) => UnsubscribeFutureState::WaitUnsubscribed,
                        Poll::Ready(Err(error)) => {
                            error!("Failed to send UNSUBSCRIBE: {}", error);
                            return Poll::Ready(Err(error));
                        }
                    }
                }

                // Step 3: Wait for a rx::Unsubscribed message from the receiver.
                UnsubscribeFutureState::WaitUnsubscribed => match self
                    .received
                    .unsubscribed
                    .lock()
                    .poll_take(cx, |msg| msg.request == self.request)
                {
                    Poll::Pending => {
                        pending = true;
                        UnsubscribeFutureState::WaitUnsubscribed
                    }
                    Poll::Ready(_) => {
                        // The router has acknowledged our unsubscription. Stop the stream and drain
                        // all remaining messages from the queue.
                        while let Poll::Ready(Some(_)) = self.subscription.poll_take() {}
                        self.task_tracker
                            .stop_subscription(self.subscription.get_subscription_id());
                        return Poll::Ready(Ok(()));
                    }
                },
            };

            if pending {
                return Poll::Pending;
            }
        }
    }
}
