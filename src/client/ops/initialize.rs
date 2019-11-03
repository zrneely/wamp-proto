use std::collections::HashMap;
use std::sync::Arc;
use std::task::Poll;

use futures::{future::poll_fn, sink::SinkExt};
use tokio::sync::oneshot;

use crate::{
    client::{Client, ClientState, ClientTaskTracker, MessageBuffer, Transport},
    error::WampError,
    pollable::PollableValue,
    proto::TxMessage,
    transport::TransportableValue as TV,
    uri::Uri,
};

pub(in crate::client) async fn initialize<T: 'static + Transport>(
    mut sink: T::Sink,
    stream: T::Stream,
    realm: Uri,
    panic_on_drop_while_open: bool,
    user_agent: Option<String>,
) -> Result<Client<T>, WampError> {
    let received = Arc::new(MessageBuffer::default());
    let (stop_sender, stop_receiver) = oneshot::channel();
    let client_state = PollableValue::new(ClientState::Establishing);

    // Build the details map, in which we describe our own capabilities
    // to the router.
    let mut details = HashMap::new();
    details.insert("roles".into(), {
        let mut roles = HashMap::new();
        if cfg!(feature = "caller") {
            roles.insert("caller".into(), TV::Dict(Default::default()));
        }
        if cfg!(feature = "callee") {
            roles.insert("callee".into(), TV::Dict(Default::default()));
        }
        if cfg!(feature = "subscriber") {
            roles.insert("subscriber".into(), TV::Dict(Default::default()));
        }
        if cfg!(feature = "publisher") {
            roles.insert("publisher".into(), TV::Dict(Default::default()));
        }
        TV::Dict(roles)
    });

    if let Some(agent) = user_agent {
        details.insert("agent".into(), TV::String(agent));
    }

    // Send the hello message.
    sink.send(TxMessage::Hello { realm, details })
        .await
        .map_err(|error| WampError::MessageSendFailed {
            message_type: "HELLO",
            error,
        })?;

    // Spawn the message listener.
    let task_tracker = ClientTaskTracker::new(sink, stop_sender);
    tokio::spawn(crate::client::message_listener::message_listener(
        task_tracker.clone(),
        stream,
        received.clone(),
        client_state.clone(),
        stop_receiver,
    ));

    // Wait for the message listener to set the client state to "Established".
    poll_fn(|cx| {
        if client_state.read(Some(cx)).is_established() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await;

    Ok(Client {
        received,

        panic_on_drop_while_open,

        state: client_state,
        task_tracker,
    })
}
