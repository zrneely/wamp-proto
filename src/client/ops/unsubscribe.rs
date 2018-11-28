use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use failure::Error;
use futures::{sync::oneshot, Async, AsyncSink, Future};
use parking_lot::RwLock;
use tokio::timer::Delay;

use client::{BroadcastHandler, Client, ClientState, ClientTaskTracker};
use error::WampError;
use proto::TxMessage;
use {Id, ReceivedValues, RouterScope, SessionScope, Transport, Uri};

#[derive(Debug)]
enum UnsubscribeFutureState {
    StartSendUnsubscribe,
    SendUnsubscribe,
    WaitUnsubscribed,
}

/// A future representing a completed unsubscription.
pub(in client) struct UnsubscriptionFuture<T: Transport> {
    state: UnsubscribeFutureState,

    subscription: Id<RouterScope>,
    timeout: Delay,

    received: ReceivedValues,
    client_state: Arc<RwLock<ClientState>>,
    task_tracker: Arc<ClientTaskTracker<T>>,
}
impl<T: Transport> UnsubscriptionFuture<T> {
    pub fn new(client: &mut Client<T>, subscription: Id<RouterScope>) -> Self {
        UnsubscriptionFuture {
            state: UnsubscribeFutureState::StartSendUnsubscribe,

            subscription,
            timeout: Delay::new(Instant::now() + client.timeout_duration),

            received: client.received.clone(),
            client_state: client.state.clone(),
            task_tracker: client.task_tracker.clone(),
        }
    }
}