use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use failure::Error;
use tokio::sync::oneshot;

use crate::{
    client::{
        Client, ClientState, ClientTaskTracker, MessageBuffer, RouterCapabilities, Transport,
    },
    pollable::PollableValue,
    proto::TxMessage,
    uri::{known_uri, Uri},
    TransportableValue as TV,
};

pub(in crate::client) fn listen_for_protocol_messages<T: Transport>(
    values: MessageBuffer,
    client_state: PollableValue<ClientState>,
    task_tracker: Arc<ClientTaskTracker<T>>,
    stop_receiver: oneshot::Receiver<()>,
) -> impl Future<Output = ()> {
    ProtocolMessageListener {
        values,
        client_state,
        task_tracker,

        state: ProtocolMessageListenerState::Ready,
        stop_receiver,
    }
}

#[derive(Debug)]
enum ProtocolMessageListenerState {
    Ready,

    PrepareReplyGoodbye,
    SendAndFlushGoodbye,

    StopAllTasks,
    CloseTransport,
}

// Used to listen for ABORT and GOODBYE messages from the router.
struct ProtocolMessageListener<T: Transport> {
    values: MessageBuffer,
    client_state: PollableValue<ClientState>,
    task_tracker: Arc<ClientTaskTracker<T>>,

    goodbye_message: Option<TxMessage>,
    transport_close_future: Option<Box<dyn Future<Output = Result<(), Error>>>>,

    state: ProtocolMessageListenerState,
    stop_receiver: oneshot::Receiver<()>,
}
impl<T: Transport> Future for ProtocolMessageListener<T> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        trace!("ProtocolMessageListener wakeup");

        loop {
            // Poll for manual shutdown.
            match self.stop_receiver.poll(cx) {
                Poll::Pending => {}
                // Either we've been told to stop or the sender was dropped,
                // in which case we should stop anyway.
                Poll::Ready(Ok(())) => {
                    debug!("ProtocolMessageListener told to stop!");
                    return Poll::Ready(());
                }
                Poll::Ready(Err(err)) => {
                    error!("ProtocolMessageListener stop listener broken: {}", err);
                    return Poll::Ready(());
                }
            }

            // Poll for transport errors
            match self.values.transport_errors.lock().poll_take(cx, |_| true) {
                Poll::Pending => {}
                Poll::Ready(error) => {
                    error!("Fatal transport error: {}", error);
                    self.client_state.write(ClientState::TransportClosed);
                    self.state = ProtocolMessageListenerState::StopAllTasks;
                }
            }

            // Notably, we don't poll for the client state changing - we'll be notified via
            // the stop listener of any significant changes to client state.

            // Process the state machine
            let mut pending = false;
            self.state = match self.state {
                ProtocolMessageListenerState::Ready => {
                    // Poll for ABORT
                    match self.values.abort.lock().poll_take(cx, |_| true) {
                        Poll::Ready(msg) => {
                            warn!(
                                "Received ABORT from router: \"{:?}\" ({:?})",
                                msg.reason, msg.details
                            );
                            self.client_state.write(ClientState::Closed);
                            ProtocolMessageListenerState::StopAllTasks
                        }

                        // Poll for GOODBYEs, but only while we're in the "Established" state. Don't eat
                        // expected GOODBYEs. We can't poll for ones that don't match the expected response
                        // to a client-initiated GOODBYE because then we leave ourselves open to a hang
                        // while connected to a badly-behaved router.
                        //
                        // Also, we don't care if the client state changes. We'll be notified via our stop
                        // listener.
                        Poll::Pending => {
                            if self.client_state.read(None) == ClientState::Established {
                                match self.values.goodbye.lock().poll_take(|_| true) {
                                    Poll::Ready(msg) => {
                                        info!(
                                            "Received GOODBYE from router: \"{:?}\" ({:?})",
                                            msg.reason, msg.details
                                        );
                                        self.client_state.write(ClientState::Closing);
                                        self.goodbye_message = Some(TxMessage::Goodbye {
                                            details: HashMap::new(),
                                            reason: Uri::raw(
                                                known_uri::session_close::goodbye_and_out
                                                    .to_string(),
                                            ),
                                        });
                                        ProtocolMessageListenerState::StartReplyGoodbye
                                    }
                                    Poll::Pending => {
                                        pending = true;
                                        ProtocolMessageListenerState::Ready
                                    }
                                }
                            } else {
                                // If we're not in the "Established" state, then act as though
                                // we polled and didn't get anything.
                                pending = true;
                                ProtocolMessageListenerState::Ready
                            }
                        }
                    }
                }

                ProtocolMessageListenerState::PrepareReplyGoodbye => {
                    debug!("ProtocolMessageListenerState sending GOODBYE response");
                    match self.task_tracker.get_sender().lock().poll_ready(cx) {
                        Poll::Pending => {
                            pending = true;
                            ProtocolMessageListenerState::PrepareReplyGoodbye
                        }
                        Poll::Ready(Ok(())) => ProtocolMessageListenerState::SendAndFlushGoodbye,
                        Poll::Ready(Err(error)) => {
                            error!("Failed to prepare to reply with GOODBYE: {}", error);
                            return Poll::Ready(());
                        }
                    }
                }

                ProtocolMessageListenerState::SendAndFlushGoodbye => {
                    if let Some(message) = self.goodbye_message.take() {
                        match self.task_tracker.get_sender().lock().start_send(message) {
                            Ok(_) => {}
                            Err(error) => {
                                error!("Failed to reply with GOODBYE: {}", error);
                                return Poll::Ready(());
                            }
                        }
                    }

                    match self.task_tracker.get_sender().lock().poll_flush(cx) {
                        Poll::Pending => {
                            pending = true;
                            ProtocolMessageListenerState::SendAndFlushGoodbye;
                        }
                        Poll::Ready(Ok(())) => ProtocolMessageListenerState::StopAllTasks,
                        Poll::Ready(Err(error)) => {
                            error!("Failed to flush when replying GOODBYE: {}", error);
                            return Poll::Ready(());
                        }
                    }
                }

                ProtocolMessageListenerState::StopAllTasks => {
                    info!("ProtocolMessageListener stopping all tasks!");
                    self.task_tracker.stop_all_except_proto_msg();

                    if self.client_state.read(None) == ClientState::TransportClosed {
                        // This is the case if the transport intitated the close by pushing to
                        // the transport_errors queue. In that case, the contract is to *not*
                        // call close() on it. Once we return, all client tasks should be stopped.
                        return Poll::Ready(());
                    } else {
                        self.transport_close_future = Some(self.task_tracker.close_transport());
                        ProtocolMessageListenerState::CloseTransport
                    }
                }

                ProtocolMessageListenerState::CloseTransport => {
                    match self.transport_close_future.as_mut().unwrap().poll() {
                        Poll::Pending => {
                            pending = true;
                            trace!(
                                "ProtocolMessageListener waiting for Transport::close to finish"
                            );
                            ProtocolMessageListenerState::CloseTransport
                        }
                        Poll::Ready(Ok(())) => {
                            self.client_state.write(ClientState::TransportClosed);
                            return Poll::Ready(());
                        }
                        Poll::Ready(Err(error)) => {
                            error!("Error driving transport close future: {}", error);
                            return Poll::Ready(());
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

pub(in crate::client) fn initialize<T: Transport>(
    mut sender: T,
    received: MessageBuffer,
    realm: Uri,
    panic_on_drop_while_open: bool,
    user_agent: Option<String>,
) -> impl Future<Output = Result<Client<T>, Error>> {
    sender.listen();

    InitializeFuture {
        state: InitializeFutureState::StartSendHello,
        message: Some(TxMessage::Hello {
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
        }),

        panic_on_drop_while_open,
        sender: Some(sender),
        received,
    }
}

#[derive(Debug)]
enum InitializeFutureState {
    PrepareSendHello,
    SendAndFlushHello,
    WaitWelcome,
}

struct InitializeFuture<T: Transport + 'static> {
    state: InitializeFutureState,
    message: Option<TxMessage>,

    // client properties
    panic_on_drop_while_open: bool,

    sender: Option<T>,
    received: MessageBuffer,
}
impl<T> Future for InitializeFuture<T>
where
    T: Transport + 'static,
{
    type Output = Result<Client<T>, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<Client<T>, Error>> {
        loop {
            trace!("InitializeFuture: {:?}", self.state);

            let mut pending = false;
            self.state = match self.state {
                // Step 1: Wait for the sender's message queue to be ready.
                InitializeFutureState::PrepareSendHello => {
                    match self.sender.as_mut().unwrap().poll_ready(cx) {
                        Poll::Pending => {
                            pending = true;
                            InitializeFutureState::PrepareSendHello
                        }
                        Poll::Ready(Ok(())) => InitializeFutureState::SendAndFlushHello,
                        Poll::Ready(Err(error)) => {
                            error!("Failed to prepare to send HELLO: {}", error);
                            return Poll::Ready(Err(error));
                        }
                    }
                }

                // Step 2: Send the message and wait for it to flush.
                InitializeFutureState::SendAndFlushHello => {
                    if let Some(message) = self.message.take() {
                        self.sender.as_mut().unwrap().start_send(message)?;
                    }

                    match self.sender.as_mut().unwrap().poll_flush(cx) {
                        Poll::Pending => {
                            pending = true;
                            InitializeFutureState::SendAndFlushHello
                        }
                        Poll::Ready(Ok(())) => InitializeFutureState::WaitWelcome,
                        Poll::Ready(Err(error)) => {
                            error!("Failed to send HELLO: {}", error);
                            return Poll::Ready(Err(error));
                        }
                    }
                }

                // Step 3: Wait for a rx::Welcome message.
                InitializeFutureState::WaitWelcome => {
                    match self.received.welcome.lock().poll_take(|_| true) {
                        Poll::Pending => {
                            pending = true;
                            InitializeFutureState::WaitWelcome
                        }
                        Poll::Ready(msg) => {
                            info!(
                                "WAMP connection established with session ID {:?}",
                                msg.session
                            );

                            let (stop_sender, receiver) = oneshot::channel();
                            let task_tracker =
                                ClientTaskTracker::new(self.sender.take().unwrap(), stop_sender);
                            let client_state = PollableValue::new(ClientState::Established);

                            tokio::spawn(listen_for_protocol_messages(
                                self.received.clone(),
                                client_state.clone(),
                                task_tracker.clone(),
                                receiver,
                            ));

                            return Ok(Poll::Ready(Client {
                                received: self.received.clone(),

                                session_id: msg.session,
                                timeout_duration: self.timeout_duration,
                                shutdown_timeout_duration: self.shutdown_timeout_duration,
                                panic_on_drop_while_open: self.panic_on_drop_while_open,
                                router_capabilities: RouterCapabilities::from_details(&msg.details),

                                // We've already sent our "hello" and received our "welcome".
                                state: client_state,
                                task_tracker,
                            }));
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
