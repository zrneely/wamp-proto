use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use failure::Error;
use futures::{Async, AsyncSink, Future};
use tokio::timer::Delay;

use client::{Client, ClientState, ClientTaskTracker};
use error::WampError;
use pollable::PollableValue;
use proto::TxMessage;
use {Broadcast, Id, SessionScope, Transport, Uri};

#[derive(Debug)]
enum PublishFutureState {
    StartSendPublish(Option<TxMessage>),
    SendPublish,
}

/// A future used to drive publishing a message
pub(in client) struct PublishFuture<T: Transport> {
    state: PublishFutureState,

    timeout: Delay,

    client_state: PollableValue<ClientState>,
    task_tracker: Arc<ClientTaskTracker<T>>,
}
impl<T: Transport> PublishFuture<T> {
    pub fn new(client: &Client<T>, topic: Uri, message: Broadcast) -> Self {
        PublishFuture {
            state: PublishFutureState::StartSendPublish(Some(TxMessage::Publish {
                request: Id::<SessionScope>::next(),
                options: HashMap::new(),
                topic,
                arguments: Some(message.arguments),
                arguments_kw: Some(message.arguments_kw),
            })),

            timeout: Delay::new(Instant::now() + client.timeout_duration),

            client_state: client.state.clone(),
            task_tracker: client.task_tracker.clone(),
        }
    }
}
impl<T: Transport> Future for PublishFuture<T> {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Result<Async<Self::Item>, Self::Error> {
        loop {
            trace!("PublishFuture: {:?}", self.state);
            ::client::check_for_timeout(&mut self.timeout)?;

            match self.client_state.read(true) {
                ClientState::Established => {},
                state => {
                    warn!("PublishFuture with unexpected client state {:?}", state);
                    return Err(WampError::InvalidClientState.into());
                }
            }

            let mut pending = false;
            self.state = match self.state {
                PublishFutureState::StartSendPublish(ref mut message) => {
                    let message = message.take().expect("invalid PublishFutureState");
                    match self.task_tracker.get_sender().lock().start_send(message)? {
                        AsyncSink::NotReady(message) => {
                            pending = true;
                            PublishFutureState::StartSendPublish(Some(message))
                        }
                        AsyncSink::Ready => PublishFutureState::SendPublish,
                    }
                }

                PublishFutureState::SendPublish => {
                    match self.task_tracker.get_sender().lock().poll_complete()? {
                        Async::NotReady => {
                            pending = true;
                            PublishFutureState::SendPublish
                        }
                        Async::Ready(_) => return Ok(Async::Ready(())),
                    }
                }
            };

            if pending {
                return Ok(Async::NotReady);
            }
        }
    }
}