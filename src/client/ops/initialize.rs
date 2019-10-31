use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use failure::Error;
use futures::{
    future::{poll_fn, FutureExt},
    pin_mut, select,
    sink::SinkExt,
};
use tokio::prelude::*;
use tokio::sync::oneshot;

use crate::{
    client::{
        Client, ClientState, ClientTaskTracker, MessageBuffer, RouterCapabilities, Transport,
    },
    pollable::PollableValue,
    proto::TxMessage,
    transport::TransportableValue as TV,
    uri::{known_uri, Uri},
};

async fn protocol_listener<T: Transport>(
    task_tracker: Arc<ClientTaskTracker<T>>,
    received: Arc<MessageBuffer>,
    client_state: PollableValue<ClientState>,
    stop_receiver: oneshot::Receiver<()>,
) {
    // We need to poll 4 sources:
    // - The stop listener
    // - Transport errors
    // - ABORT messages
    // - GOODBYE messages (if state == Established)

    let stop_listener = poll_fn(|cx| stop_receiver.poll_unpin(cx)).fuse();
    let transport_error_listener =
        poll_fn(|cx| received.transport_errors.poll_take(cx, |_| true)).fuse();
    let abort_message_listener = poll_fn(|cx| received.abort.poll_take(cx, |_| true)).fuse();
    let goodbye_message_listener = poll_fn(|cx| received.goodbye.poll_take(cx, |_| true)).fuse();

    pin_mut!(
        stop_listener,
        transport_error_listener,
        abort_message_listener,
        goodbye_message_listener
    );

    // This "select" is the reason the recursion limit is set to 512 in lib.rs.
    select! {
        sl = stop_listener => {
            // When the stop listener fires, we immediately exit.
            if let Err(error) = sl {
                error!("Protocol message stop listener errored: {}", error);
            }
        }

        transport_err = transport_error_listener => {
            // When we receive a transport error, set the client state to TransportClosed
            // and stop all tasks. We explicitly do NOT close() the Transport.
            error!("Transport error received: {}", transport_err);
            client_state.write(ClientState::TransportClosed);
            task_tracker.stop_all_except_proto_msg();
        }

        abort_msg = abort_message_listener => {
            // When we receive an ABORT message, we need to set the client state to Closed,
            // stop all tasks, close the transport, and finally set the state to TransportClosed.
            warn!("ABORT message received. Reason: {}, details: {:?}", abort_msg.reason, abort_msg.details);
            client_state.write(ClientState::Closed);
            task_tracker.stop_all_except_proto_msg();
            task_tracker.get_sender().await.close().await;
            client_state.write(ClientState::TransportClosed);
        }

        goodbye_msg = goodbye_message_listener => {
            // Only pay attention if the current state is Established.
            info!("GOODBYE message received. Reason: {}, details: {:?}", goodbye_msg.reason, goodbye_msg.details);

            if client_state.read(None) == ClientState::Established {
                // We need to set the client state to Closing, send a GOODBYE response, stop all
                // tasks, close the transport, and set the client state to TransportClosed.
                client_state.write(ClientState::Closing);

                {
                    let sender = task_tracker.get_sender().await;
                    poll_fn(|cx| task_tracker.poll_ready(cx)).await?;
                    sender.start_send(TxMessage::Goodbye {
                        details: HashMap::new(),
                        reason: Uri::raw(
                            known_uri::session_close::goodbye_and_out
                                .to_string(),
                        )
                    })?;
                }

                {
                    let sender = task_tracker.get_sender().await;
                    poll_fn(|cx| task_tracker.poll_flush(cx)).await?;
                }

                task_tracker.stop_all_except_proto_msg();
                task_tracker.get_sender().await.close().await;
                client_state.write(ClientState::TransportClosed);
            } else {
                warn!("Ignoring GOODBYE message since client state is not Established");
            }
        }
    }
}

pub(in crate::client) async fn initialize<T: 'static + Transport>(
    mut sender: Pin<Box<T>>,
    received: Arc<MessageBuffer>,
    realm: Uri,
    panic_on_drop_while_open: bool,
    user_agent: Option<String>,
) -> Result<Client<T>, Error> {
    sender.listen(); // TODO: should this move into Transport::connect?

    // Send the hello message.
    sender
        .as_mut()
        .send(TxMessage::Hello {
            realm,
            details: {
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

                details
            },
        })
        .await?;

    // Wait for the response.
    let welcome_msg = poll_fn(|cx| received.welcome.poll_take(cx, |_| true)).await;

    info!(
        "WAMP connection established with session ID {:?}",
        welcome_msg.session
    );

    let (stop_sender, stop_receiver) = oneshot::channel();
    let task_tracker = ClientTaskTracker::new(sender, stop_sender);
    let client_state = PollableValue::new(ClientState::Established);

    tokio::spawn(protocol_listener(
        task_tracker.clone(),
        received.clone(),
        client_state.clone(),
        stop_receiver,
    ));

    Ok(Client {
        received,
        session_id: welcome_msg.session,

        panic_on_drop_while_open,
        router_capabilities: RouterCapabilities::from_details(&welcome_msg.details),

        state: client_state,
        task_tracker,
    })
}
