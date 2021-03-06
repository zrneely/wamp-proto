use std::collections::HashMap;
use std::sync::Arc;

use futures::{future::poll_fn, pin_mut, select, FutureExt, SinkExt, StreamExt};
use tokio::sync::oneshot;

use crate::{
    client::{ClientState, ClientTaskTracker, RouterCapabilities},
    pollable::PollableValue,
    proto::{msg_code, rx::RxMessage, TxMessage},
    transport::{Transport, TransportError},
    uri::known_uri,
    GlobalScope, Id, MessageBuffer,
};

/// This is a long-running task (only finishes when the client is closed)
/// which receives and interprets messages from the transport, adding them
/// to the provided [`MessageBuffer`] as they come.
///
/// Because WAMP messages can be received in any order and different peer/peer
/// interactions can interleave themselves, a message buffer is needed.
pub(in crate::client) async fn message_listener<T: Transport>(
    task_tracker: Arc<ClientTaskTracker<T>>,
    stream: T::Stream,
    received: Arc<MessageBuffer>,
    client_state: PollableValue<ClientState>,
    mut stop_receiver: oneshot::Receiver<()>,
) {
    let stop_listener = poll_fn(|cx| stop_receiver.poll_unpin(cx)).fuse();
    let message_listener =
        message_listener_impl(task_tracker, stream, received, client_state).fuse();

    pin_mut!(stop_listener, message_listener);
    select! {
        sl = stop_listener => match sl {
            Ok(_) => {}
            Err(err) => {
                error!("message listener's stop listener was broken: {}", err);
            }
        },
        ml = message_listener => {}
    }
}

// Each isolated chunk is relatively simple, and it would be a hassle to pass all
// the arguments on the stack to helper functions.
#[allow(clippy::cognitive_complexity)]
async fn message_listener_impl<T: Transport>(
    task_tracker: Arc<ClientTaskTracker<T>>,
    mut stream: T::Stream,
    received: Arc<MessageBuffer>,
    client_state: PollableValue<ClientState>,
) {
    // Poll the stream for new messages forever (or until we are stopped).
    'msg_loop: loop {
        match poll_fn(|cx| stream.poll_next_unpin(cx)).await {
            Some(stream_value) => {
                match process_stream_value(stream_value, &received, client_state.read(None)) {
                    ProcessMessageResult::Continue => {}
                    ProcessMessageResult::Welcome {
                        session_id,
                        router_capabilities,
                    } => {
                        info!(
                            "WAMP connection established with session ID {:?}. Router Capabilities: {:?}",
                            session_id, router_capabilities
                        );
                        client_state.write(ClientState::Established {
                            session_id,
                            router_capabilities,
                        });
                    }
                    ProcessMessageResult::Abort => {
                        // When we receive an ABORT message, we need to set the client state to Closed,
                        // stop all tasks, close the transport, set the state to TransportClosed, and finish.
                        client_state.write(ClientState::Closed);
                        task_tracker.stop_all_except_message_listener();
                        if let Err(error) = task_tracker.get_sender().lock().await.close().await {
                            error!("Failed to close sender when handling ABORT: {}", error);
                        }
                        client_state.write(ClientState::TransportClosed);
                        return;
                    }
                    ProcessMessageResult::ProtocolError => {
                        // When a protocol error occurs, we need to set the client state to Failed,
                        // send an ABORT message, stop all tasks, close the transport,
                        // set the state to TransportClosed, and finish.
                        client_state.write(ClientState::Failed);
                        if let Err(error) = {
                            task_tracker
                                .get_sender()
                                .lock()
                                .await
                                .send(TxMessage::Abort {
                                    details: HashMap::default(),
                                    reason: known_uri::protocol_violation,
                                })
                                .await
                        } {
                            error!("Failed to send ABORT message in response to protocol violation: {}", error);
                        }

                        task_tracker.stop_all_except_message_listener();
                        if let Err(error) = task_tracker.get_sender().lock().await.close().await {
                            error!(
                                "Failed to close sender in response to protocol violation: {}",
                                error
                            );
                        }
                        client_state.write(ClientState::TransportClosed);
                        return;
                    }
                    ProcessMessageResult::Goodbye => {
                        // Ignore GOODBYE messages unless the current state is Established.
                        if !client_state.read(None).is_established() {
                            warn!("Ignoring GOODBYE since state is not Established");
                            continue 'msg_loop;
                        }

                        // We need to set the client state to Closing, send a GOODBYE response,
                        // stop all tasks, close the transport, set the state to TransportClosed,
                        // and finish.
                        client_state.write(ClientState::Closing);
                        if let Err(error) = {
                            task_tracker
                                .get_sender()
                                .lock()
                                .await
                                .send(TxMessage::Goodbye {
                                    details: HashMap::default(),
                                    reason: known_uri::session_close::goodbye_and_out,
                                })
                                .await
                        } {
                            error!("Failed to respond to GOODBYE message: {}", error);
                        }
                        task_tracker.stop_all_except_message_listener();
                        if let Err(error) = task_tracker.get_sender().lock().await.close().await {
                            error!("Failed to close sender when handling GOODBYE: {}", error);
                        }
                        client_state.write(ClientState::TransportClosed);
                        return;
                    }
                }
            }
            None => {
                info!("Transport stream terminated!");
                return;
            }
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum ProcessMessageResult {
    // Processed a normal message; continue as usual
    Continue,
    // Establish a session
    Welcome {
        session_id: Id<GlobalScope>,
        router_capabilities: RouterCapabilities,
    },
    // Immediately shut down
    Abort,
    // Clean remote-triggered disconnect
    Goodbye,
    // Send ABORT and shut down
    ProtocolError,
}

fn process_stream_value(
    value: Result<RxMessage, TransportError>,
    received: &MessageBuffer,
    client_state: ClientState,
) -> ProcessMessageResult {
    match value {
        Err(TransportError::NetworkError(error)) => {
            error!("Transport reported network error: {}", error);
            ProcessMessageResult::Abort
        }

        Err(TransportError::ParseError(error)) => {
            error!("Transport reported message parse error: {}", error);
            ProcessMessageResult::ProtocolError
        }

        Ok(message) => process_message(message, received, client_state),
    }
}

// Although there are lots of branches, most of them are very straightforward.
#[allow(clippy::cognitive_complexity)]
fn process_message(
    message: RxMessage,
    received: &MessageBuffer,
    client_state: ClientState,
) -> ProcessMessageResult {
    match message {
        RxMessage::Welcome(welcome) => {
            if client_state.is_established() {
                warn!("Received WELCOME with active session: {:?}", welcome);
                ProcessMessageResult::ProtocolError
            } else {
                trace!("Received WELCOME: {:?}", welcome);
                ProcessMessageResult::Welcome {
                    session_id: welcome.session,
                    router_capabilities: RouterCapabilities::from_details(&welcome.details),
                }
            }
        }

        RxMessage::Abort(abort) => {
            trace!("Received ABORT: {:?}", abort);
            ProcessMessageResult::Abort
        }

        RxMessage::Goodbye(goodbye) => match client_state {
            ClientState::Established { .. } => {
                trace!("Received GOODBYE: {:?}", goodbye);
                ProcessMessageResult::Goodbye
            }
            ClientState::ShuttingDown => {
                trace!(
                    "Received GOODBYE after client-initiated shutdown: {:?}",
                    goodbye
                );
                received.goodbye.insert(goodbye);
                ProcessMessageResult::Continue
            }
            _ => {
                warn!("Received GOODBYE with no active session: {:?}", goodbye);
                ProcessMessageResult::ProtocolError
            }
        },

        RxMessage::Error(error) => {
            if client_state.is_established() {
                match error.request_type {
                    #[cfg(feature = "subscriber")]
                    msg_code::SUBSCRIBE => {
                        trace!("Received SUBSCRIBE ERROR: {:?}", error);
                        received.errors.subscribe.insert(error);
                        ProcessMessageResult::Continue
                    }

                    #[cfg(feature = "subscriber")]
                    msg_code::UNSUBSCRIBE => {
                        trace!("Received UNSUBSCRIBE ERROR: {:?}", error);
                        received.errors.unsubscribe.insert(error);
                        ProcessMessageResult::Continue
                    }

                    #[cfg(feature = "publisher")]
                    msg_code::PUBLISH => {
                        trace!("Received PUBLISH ERROR: {:?}", error);
                        received.errors.publish.insert(error);
                        ProcessMessageResult::Continue
                    }

                    #[cfg(feature = "callee")]
                    msg_code::REGISTER => {
                        trace!("Received REGISTER ERROR: {:?}", error);
                        received.errors.register.insert(error);
                        ProcessMessageResult::Continue
                    }

                    #[cfg(feature = "callee")]
                    msg_code::UNREGISTER => {
                        trace!("Received UNREGISTER ERROR: {:?}", error);
                        received.errors.unregister.insert(error);
                        ProcessMessageResult::Continue
                    }

                    #[cfg(feature = "caller")]
                    msg_code::CALL => {
                        trace!("Received CALL ERROR: {:?}", error);
                        received.errors.call.insert(error);
                        ProcessMessageResult::Continue
                    }

                    _ => {
                        warn!("Received ERROR with invalid request type: {:?}", error);
                        ProcessMessageResult::ProtocolError
                    }
                }
            } else {
                warn!("Received ERROR with no active session: {:?}", error);
                ProcessMessageResult::ProtocolError
            }
        }

        #[cfg(feature = "subscriber")]
        RxMessage::Subscribed(subscribed) => {
            if client_state.is_established() {
                trace!("Received SUBSCRIBED: {:?}", subscribed);
                received.subscribed.insert(subscribed);
                ProcessMessageResult::Continue
            } else {
                warn!(
                    "Received SUBSCRIBED with no active session: {:?}",
                    subscribed
                );
                ProcessMessageResult::ProtocolError
            }
        }

        #[cfg(feature = "subscriber")]
        RxMessage::Unsubscribed(unsubscribed) => {
            if client_state.is_established() {
                trace!("Received UNSUBSCRIBED: {:?}", unsubscribed);
                received.unsubscribed.insert(unsubscribed);
                ProcessMessageResult::Continue
            } else {
                warn!(
                    "Received UNSUBSCRIBED with no active session: {:?}",
                    unsubscribed
                );
                ProcessMessageResult::ProtocolError
            }
        }

        #[cfg(feature = "subscriber")]
        RxMessage::Event(event) => {
            if client_state.is_established() {
                if let Some(event_queue) = received.event.read().get(&event.subscription) {
                    trace!("Received EVENT: {:?}", event);
                    event_queue.insert(event);
                    ProcessMessageResult::Continue
                } else {
                    warn!("Received EVENT for unknown subscription ID: {:?}", event);
                    ProcessMessageResult::ProtocolError
                }
            } else {
                warn!("Received EVENT with no active session: {:?}", event);
                ProcessMessageResult::ProtocolError
            }
        }

        #[cfg(feature = "publisher")]
        RxMessage::Published(published) => {
            if client_state.is_established() {
                trace!("Received PUBLISHED: {:?}", published);
                received.published.insert(published);
                ProcessMessageResult::Continue
            } else {
                warn!("Received PUBLISHED with no active session: {:?}", published);
                ProcessMessageResult::ProtocolError
            }
        }

        #[cfg(feature = "callee")]
        RxMessage::Registered(registered) => {
            if client_state.is_established() {
                trace!("Received REGISTERED: {:?}", registered);
                received.registered.insert(registered);
                ProcessMessageResult::Continue
            } else {
                warn!(
                    "Received REGISTERED with no active session: {:?}",
                    registered
                );
                ProcessMessageResult::ProtocolError
            }
        }

        #[cfg(feature = "callee")]
        RxMessage::Unregistered(unregistered) => {
            if client_state.is_established() {
                trace!("Received UNREGISTERED: {:?}", unregistered);
                received.unregistered.insert(unregistered);
                ProcessMessageResult::Continue
            } else {
                warn!(
                    "Receieved UNREGISTERED with no active session: {:?}",
                    unregistered
                );
                ProcessMessageResult::ProtocolError
            }
        }

        #[cfg(feature = "callee")]
        RxMessage::Invocation(invocation) => {
            if client_state.is_established() {
                trace!("Received INVOCATION: {:?}", invocation);
                received.invocation.insert(invocation);
                ProcessMessageResult::Continue
            } else {
                warn!(
                    "Received INVOCATION with no active session: {:?}",
                    invocation
                );
                ProcessMessageResult::ProtocolError
            }
        }

        #[cfg(feature = "caller")]
        RxMessage::Result(result) => {
            if client_state.is_established() {
                trace!("Received RESULT: {:?}", result);
                received.result.insert(result);
                ProcessMessageResult::Continue
            } else {
                warn!("Received RESULT with no active session: {:?}", result);
                ProcessMessageResult::ProtocolError
            }
        }
    }
}
