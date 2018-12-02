use std::collections::HashMap;
use std::sync::Arc;

use failure::Error;
use futures::{
    future::{self, Either, Future},
    sink::Sink,
    stream::{SplitSink, SplitStream, Stream},
    sync::oneshot,
    Async, AsyncSink,
};
use http::HeaderMap;
use parking_lot::Mutex;
use serde_json::{self, Value};
use tokio::reactor;
use websocket::{
    async::{client::Client, TcpStream},
    message::{Message, OwnedMessage},
    ClientBuilder,
};

use error::WampError;
use proto::{
    rx::{self, RxMessage},
    TxMessage,
};
use {
    GlobalScope, Id, ReceivedValues, RouterScope, SessionScope, Transport, TransportableValue, Uri,
};

/// An implementation of a websocket-based WAMP Transport.
pub struct WebsocketTransport {
    // These are both replaced with None and dropped by close().
    client: Option<SplitSink<Client<TcpStream>>>,
    stream: Arc<Mutex<Option<SplitStream<Client<TcpStream>>>>>,

    // Used to tell the WebsocketTransportListener to finish. If
    // it's None, then listen() hasn't been called yet.
    listener_stop_sender: Option<oneshot::Sender<()>>,

    // Only present until listen() has been called once
    received_values: Option<ReceivedValues>,
}
impl Sink for WebsocketTransport {
    type SinkItem = TxMessage;
    type SinkError = Error;

    fn start_send(
        &mut self,
        item: Self::SinkItem,
    ) -> Result<AsyncSink<Self::SinkItem>, Self::SinkError> {
        let value = item.to_json();
        if let Some(ref mut client) = self.client {
            let message = Message::text(serde_json::to_string(&value)?).into();
            client
                .start_send(message)
                .map(|async| match async {
                    AsyncSink::NotReady(_) => AsyncSink::NotReady(item),
                    AsyncSink::Ready => AsyncSink::Ready,
                }).map_err(|e| e.into())
        } else {
            Err(WampError::TransportStreamClosed.into())
        }
    }

    fn poll_complete(&mut self) -> Result<Async<()>, Self::SinkError> {
        if let Some(ref mut client) = self.client {
            client.poll_complete().map_err(|e| e.into())
        } else {
            Err(WampError::TransportStreamClosed.into())
        }
    }
}
impl Transport for WebsocketTransport {
    type ConnectFuture = WebsocketTransportConnectFuture;
    type CloseFuture = WebsocketTransportCloseFuture;

    fn connect(url: &str, received_values: ReceivedValues) -> WebsocketTransportConnectFuture {
        let future = match ClientBuilder::new(url) {
            Ok(builder) => Either::A(
                builder
                    .async_connect_insecure(&reactor::Handle::current())
                    .map_err(|e| e.into()),
            ),
            Err(e) => Either::B(future::err(e.into())),
        };

        WebsocketTransportConnectFuture {
            future: Box::new(future),
            received_values,
        }
    }

    fn listen(&mut self) {
        if let (stream, Some(received_values)) = (self.stream.clone(), self.received_values.take())
        {
            let (sender, stop_receiver) = oneshot::channel();
            self.listener_stop_sender = Some(sender);
            tokio::spawn(WebsocketTransportListener {
                stream,
                received_values,
                stop_receiver,
            });
        } else {
            warn!("WebsocketTransport::listen called multiple times!");
        }
    }

    fn close(&mut self) -> Self::CloseFuture {
        // Dropping the stream will close the listener; we also spawn a new task to close the client.
        self.stream.lock().take();
        debug!("WebsocketTransport dropped stream");

        // If the listener is started, stop it.
        if let Some(sender) = self.listener_stop_sender.take() {
            let _ = sender.send(());
            debug!("WebsocketTransport sent stop signal to WebsocketTransportListener");
        }

        WebsocketTransportCloseFuture {
            client: self.client.take(),
        }
    }
}
impl Drop for WebsocketTransport {
    fn drop(&mut self) {
        if let Some(sender) = self.listener_stop_sender.take() {
            let _ = sender.send(());
            warn!("WebsocketTransport sent stop signal to WebsocketTransportListener in drop!");
        }
    }
}

struct WebsocketTransportListener {
    stream: Arc<Mutex<Option<SplitStream<Client<TcpStream>>>>>,
    received_values: ReceivedValues,
    stop_receiver: oneshot::Receiver<()>,
}
impl Future for WebsocketTransportListener {
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Result<Async<Self::Item>, Self::Error> {
        self.poll_impl().map_err(|e| {
            // If we get a transport error, tell the ProtocolMessageListener to stop and drop
            // our underlying stream. The client will not call close() on us.
            error!("WebsocketTransportListener poll error: {:?}", e);
            self.stream.lock().take();
            self.received_values.transport_errors.lock().insert(e);
        })
    }
}
impl WebsocketTransportListener {
    fn poll_impl(&mut self) -> Result<Async<()>, Error> {
        trace!("WebsocketTransportListener wakeup");
        loop {
            match self.stop_receiver.poll() {
                // we haven't been told to stop
                Ok(Async::NotReady) => {}
                // either we have been told to stop, or the sender was dropped, in which case
                // we should also stop
                Ok(Async::Ready(_)) | Err(_) => {
                    debug!("WebsocketTransportListener told to stop!");
                    return Ok(Async::Ready(()));
                }
            }

            let poll_result = {
                if let Some(ref mut stream) = *self.stream.lock() {
                    stream.poll()?
                } else {
                    warn!("WebsocketTransportListener source stream closed!");
                    return Ok(Async::Ready(()));
                }
            };

            match poll_result {
                // Happy path
                Async::Ready(Some(OwnedMessage::Text(message))) => self.handle_message(message),

                // Received some non-text message: log and move on
                Async::Ready(Some(message)) => {
                    warn!("Received non-text message {:?}", message);
                }

                // Received no message: stream is closed
                Async::Ready(None) => {
                    warn!("Websocket underlying stream closed!");
                    return Ok(Async::Ready(()));
                }

                // Nothing available
                Async::NotReady => return Ok(Async::NotReady),
            }
        }
    }

    fn handle_message(&mut self, message: String) {
        if let Ok(Value::Array(vals)) = serde_json::from_str(&message) {
            debug!("Received websocket message: {:?}", vals);
            if vals.len() > 0 {
                if let Some(code) = vals[0].as_u64() {
                    match code {
                        rx::Welcome::MSG_CODE => self.handle_welcome(&vals[1..]),
                        rx::Abort::MSG_CODE => self.handle_abort(&vals[1..]),
                        rx::Goodbye::MSG_CODE => self.handle_goodbye(&vals[1..]),
                        rx::Subscribed::MSG_CODE => self.handle_subscribed(&vals[1..]),
                        rx::Unsubscribed::MSG_CODE => self.handle_unsubscribed(&vals[1..]),
                        rx::Event::MSG_CODE => self.handle_event(&vals[1..]),

                        _ => {
                            warn!("Received unknown message code {}", code);
                        }
                    }
                } else {
                    warn!("Received non-integer message code {:?}", vals[0]);
                }
            } else {
                warn!("Received zero-length message");
            }
        } else {
            warn!("Received bad or non-array JSON {}", message);
        }
    }

    fn handle_welcome(&mut self, msg: &[Value]) {
        if msg.len() != 2 {
            warn!("Bad WELCOME message length");
            return;
        }

        let session = if let Some(session_id) = msg[0].as_u64() {
            Id::<GlobalScope>::from_raw_value(session_id)
        } else {
            warn!("Bad WELCOME message session ID {:?}", msg[0]);
            return;
        };

        let details = if let Some(TransportableValue::Dict(details)) = json_to_tv(&msg[1]) {
            details
        } else {
            warn!("Bad WELCOME message details {:?}", msg[1]);
            return;
        };

        debug!("Adding WELCOME message: {:?}, {:?}", session, details);
        self.received_values
            .welcome
            .lock()
            .insert(rx::Welcome { session, details });
    }

    fn handle_abort(&mut self, msg: &[Value]) {
        if msg.len() != 2 {
            warn!("Bad ABORT message length");
            return;
        }

        let details = if let Some(TransportableValue::Dict(details)) = json_to_tv(&msg[0]) {
            details
        } else {
            warn!("Bad ABORT message details {:?}", msg[0]);
            return;
        };

        let reason = if let Some(uri_str) = msg[1].as_str() {
            if let Some(reason) = Uri::relaxed(uri_str) {
                reason
            } else {
                warn!("Bad URI in ABORT message reason {:?}", msg[1]);
                return;
            }
        } else {
            warn!("Bad ABORT message reason {:?}", msg[1]);
            return;
        };

        debug!("Adding ABORT message: {:?} {:?}", details, reason);
        self.received_values
            .abort
            .lock()
            .insert(rx::Abort { details, reason });
    }

    fn handle_goodbye(&mut self, msg: &[Value]) {
        if msg.len() != 2 {
            warn!("Bad GOODBYE message length");
            return;
        }

        let details = if let Some(TransportableValue::Dict(details)) = json_to_tv(&msg[0]) {
            details
        } else {
            warn!("Bad GOODBYE message details {:?}", msg[0]);
            return;
        };

        let reason = if let Some(uri_str) = msg[1].as_str() {
            if let Some(reason) = Uri::relaxed(uri_str) {
                reason
            } else {
                warn!("Bad URI in GOODBYE message reason {:?}", msg[1]);
                return;
            }
        } else {
            warn!("Bad GOODBYE message reason {:?}", msg[1]);
            return;
        };

        debug!("Adding GOODBYE message: {:?} {:?}", details, reason);
        self.received_values
            .goodbye
            .lock()
            .insert(rx::Goodbye { details, reason });
    }

    fn handle_subscribed(&mut self, msg: &[Value]) {
        if msg.len() != 2 {
            warn!("Bad SUBSCRIBED message length");
            return;
        }

        let request = if let Some(request_id_raw) = msg[0].as_u64() {
            Id::<SessionScope>::from_raw_value(request_id_raw)
        } else {
            warn!("Bad SUBSCRIBED message request ID {:?}", msg[0]);
            return;
        };

        let subscription = if let Some(subscription_id_raw) = msg[1].as_u64() {
            Id::<RouterScope>::from_raw_value(subscription_id_raw)
        } else {
            warn!("Bad SUBSCRIBED message subscription ID {:?}", msg[1]);
            return;
        };

        debug!(
            "Adding SUBSCRIBED message: {:?} {:?}",
            request, subscription
        );
        self.received_values
            .subscribed
            .lock()
            .insert(rx::Subscribed {
                request,
                subscription,
            });
    }

    fn handle_unsubscribed(&mut self, msg: &[Value]) {
        if msg.len() != 1 {
            warn!("Bad UNSUBSCRIBED message length");
            return;
        }

        let request = if let Some(request_id_raw) = msg[0].as_u64() {
            Id::<SessionScope>::from_raw_value(request_id_raw)
        } else {
            warn!("Bad UNSUBSCRIBED message request ID {:?}", msg[0]);
            return;
        };

        debug!("Adding UNSUBSCRIBED message: {:?}", request);
        self.received_values
            .unsubscribed
            .lock()
            .insert(rx::Unsubscribed { request });
    }

    fn handle_event(&mut self, msg: &[Value]) {
        if msg.len() < 3 || msg.len() > 5 {
            warn!("Bad EVENT message length");
            return;
        }

        let subscription = if let Some(subscription_id) = msg[0].as_u64() {
            Id::<RouterScope>::from_raw_value(subscription_id)
        } else {
            warn!("Bad EVENT subscription ID {:?}", msg[0]);
            return;
        };

        let publication = if let Some(publication_id) = msg[1].as_u64() {
            Id::<GlobalScope>::from_raw_value(publication_id)
        } else {
            warn!("Bad EVENT publication ID {:?}", msg[1]);
            return;
        };

        let details = if let Some(TransportableValue::Dict(details)) = json_to_tv(&msg[2]) {
            details
        } else {
            warn!("Bad EVENT details {:?}", msg[2]);
            return;
        };

        let arguments = if msg.len() > 3 {
            if let Some(TransportableValue::List(arguments)) = json_to_tv(&msg[3]) {
                Some(arguments)
            } else {
                warn!("Bad EVENT arguments {:?}", msg[3]);
                return;
            }
        } else {
            None
        };

        let arguments_kw = if msg.len() > 4 {
            if let Some(TransportableValue::Dict(arguments_kw)) = json_to_tv(&msg[4]) {
                Some(arguments_kw)
            } else {
                warn!("Bad EVENT arguments_kw {:?}", msg[4]);
                return;
            }
        } else {
            None
        };

        debug!(
            "Adding EVENT message: {:?} {:?} {:?} {:?} {:?}",
            subscription, publication, details, arguments, arguments_kw
        );
        self.received_values.event.lock().insert(rx::Event {
            subscription,
            publication,
            details,
            arguments,
            arguments_kw,
        });
    }
}

fn json_to_tv(value: &Value) -> Option<TransportableValue> {
    Some(match value {
        Value::Null => return None,
        Value::Bool(val) => TransportableValue::Bool(*val),
        Value::Number(num) => if let Some(val) = num.as_u64() {
            TransportableValue::Integer(val)
        } else {
            warn!("Skipping negative or floating point number {:?}", num);
            return None;
        },
        Value::String(val) => TransportableValue::String(val.clone()),
        Value::Array(vals) => {
            TransportableValue::List(vals.into_iter().filter_map(json_to_tv).collect())
        }
        Value::Object(vals) => {
            let mut result = HashMap::<String, TransportableValue>::new();
            for (k, v) in vals {
                if let Some(val) = json_to_tv(v) {
                    result.insert(k.clone(), val);
                }
            }
            TransportableValue::Dict(result)
        }
    })
}

/// Returned by [`WebsocketTransport::connect`]; resolves to a [`WebsocketTransport`].
pub struct WebsocketTransportConnectFuture {
    // Sadly we have to box this value, since it's type isn't nameable.
    future: Box<Future<Item = (Client<TcpStream>, HeaderMap), Error = Error> + Send>,
    received_values: ReceivedValues,
}
impl Future for WebsocketTransportConnectFuture {
    type Item = WebsocketTransport;
    type Error = Error;

    fn poll(&mut self) -> Result<Async<Self::Item>, Self::Error> {
        // If the client is ready, make a transport
        match self.future.poll()? {
            Async::NotReady => return Ok(Async::NotReady),
            Async::Ready((client, _headers)) => {
                let (client, stream) = client.split();

                return Ok(Async::Ready(WebsocketTransport {
                    client: Some(client),
                    stream: Arc::new(Mutex::new(Some(stream))),
                    received_values: Some(self.received_values.clone()),
                    listener_stop_sender: None,
                }));
            }
        }
    }
}

/// Returned by [`WebsocketTransport::close`]; resolves to nothing.
pub struct WebsocketTransportCloseFuture {
    client: Option<SplitSink<Client<TcpStream>>>,
}
impl Future for WebsocketTransportCloseFuture {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Result<Async<Self::Item>, Self::Error> {
        trace!("WebsocketTransportCloseFuture wakeup");
        if let Some(ref mut client) = self.client {
            match client.close() {
                Ok(v) => Ok(v),
                Err(e) => {
                    warn!("Error in WebsocketTransportCloseFuture: {:?}", e);
                    Err(e.into())
                }
            }
        } else {
            debug!("WebsocketTransportCloseFuture has no client!");
            Ok(Async::Ready(()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future::poll_fn;
    use tokio::runtime::current_thread;

    use parking_lot::Mutex;
    use std::io::{self, prelude::*};
    use std::sync::Arc;

    #[derive(Clone)]
    struct MockRead {
        buf: Arc<Mutex<Vec<u8>>>,
        return_val: Arc<Mutex<Option<io::Error>>>,
        read_called: Arc<Mutex<bool>>,
    }
    impl Read for MockRead {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, io::Error> {
            if let Some(e) = self.return_val.lock().take() {
                Err(e)
            } else {
                let mut lock = self.buf.lock();
                let len = buf.len();
                buf.copy_from_slice(&lock.drain(0..len).collect::<Vec<_>>());
                *self.read_called.lock() = true;
                Ok(len)
            }
        }
    }
    impl ::tokio::io::AsyncRead for MockRead {}
    impl Default for MockRead {
        fn default() -> Self {
            MockRead {
                buf: Arc::new(Mutex::new(vec![])),
                return_val: Arc::new(Mutex::new(None)),
                read_called: Arc::new(Mutex::new(false)),
            }
        }
    }

    #[test]
    fn json_to_tv_test_integer() {
        assert_eq!(Some(TransportableValue::Integer(4)), json_to_tv(&json!(4)));
        assert_eq!(Some(TransportableValue::Integer(0)), json_to_tv(&json!(0)));
        assert_eq!(
            Some(TransportableValue::Integer(0x7FFF_FFFF_FFFF_FFFF)),
            json_to_tv(&json!(0x7FFF_FFFF_FFFF_FFFFu64))
        );
        assert_eq!(None, json_to_tv(&json!(-1)));
        assert_eq!(None, json_to_tv(&json!(-3)));
    }

    #[test]
    fn json_to_tv_test_array() {
        assert_eq!(
            Some(TransportableValue::List(vec![
                TransportableValue::Integer(1),
                TransportableValue::Integer(2),
                TransportableValue::String("asdf".into())
            ])),
            json_to_tv(&json!([1, 2, "asdf"]))
        );
    }

    #[test]
    fn handle_welcome_test() {
        let rv = ReceivedValues::default();
        let (_sender, stop_receiver) = oneshot::channel();
        let mut listener = WebsocketTransportListener {
            stream: unsafe { ::std::mem::uninitialized() },
            received_values: rv.clone(),
            stop_receiver,
        };

        println!("Scenario 0: happy path");
        listener.handle_welcome(&[json!(12345), json!({"x": 1, "y": true})]);
        assert_eq!(1, rv.welcome.lock().len());
        assert_eq!(1, rv.len());
        let query = poll_fn(|| {
            let val = try_ready!({
                let res: Result<_, &'static str> = Ok(rv.welcome.lock().poll_take(|_| true));
                res
            });
            if Id::<GlobalScope>::from_raw_value(12345) != val.session {
                return Err(format!("session id {:?} did not match", val.session));
            }
            if 2 != val.details.len() {
                return Err(format!("details dict {:?} did not match", val.details));
            }
            Ok(Async::Ready(()))
        });
        assert_eq!(current_thread::block_on_all(query), Ok(()));

        println!("Scenario 1: 'Session' is not a number");
        listener.handle_welcome(&[json!("foobar"), json!({"x": 1, "y": true})]);
        assert_eq!(0, rv.len());

        println!("Scenario 2: 'Details' is not a dictionary");
        listener.handle_welcome(&[json!(12345), json!("foobar")]);
        assert_eq!(0, rv.len());

        println!("Scenario 3: Not enough arguments");
        listener.handle_welcome(&[json!(12345)]);
        assert_eq!(0, rv.len());

        println!("Scenario 4: Too many arguments");
        listener.handle_welcome(&[json!(12345), json!({"x": 1, "y": true}), json!("foobar")]);
        assert_eq!(0, rv.len());
    }

    #[test]
    fn handle_abort_test() {
        let rv = ReceivedValues::default();
        let (_sender, stop_receiver) = oneshot::channel();
        let mut listener = WebsocketTransportListener {
            stream: unsafe { ::std::mem::uninitialized() },
            received_values: rv.clone(),
            stop_receiver,
        };

        println!("Scenario 0: happy path");
        listener.handle_abort(&[json!({"x": 1, "y": true}), json!("org.foo.bar.error")]);
        assert_eq!(1, rv.abort.lock().len());
        assert_eq!(1, rv.len());
        let query = poll_fn(|| {
            let val = try_ready!({
                let res: Result<_, &'static str> = Ok(rv.abort.lock().poll_take(|_| true));
                res
            });
            if 2 != val.details.len() {
                return Err(format!("details dict {:?} did not match", val.details));
            }
            if "org.foo.bar.error" != val.reason.to_raw() {
                return Err(format!("reason URI {:?} did not match", val.reason));
            }
            Ok(Async::Ready(()))
        });
        assert_eq!(current_thread::block_on_all(query), Ok(()));

        println!("Scenario 1: 'Details' is not a dictionary");
        listener.handle_abort(&[json!("foobar"), json!("org.foo.bar.error")]);
        assert_eq!(0, rv.len());

        println!("Scenario 2: 'Reason' is not a string");
        listener.handle_abort(&[json!({"x": 1, "y": true}), json!(12345)]);
        assert_eq!(0, rv.len());

        println!("Scenario 3: 'Reason' is not a valid URI");
        listener.handle_abort(&[json!({"x": 1, "y": true}), json!("..")]);
        assert_eq!(0, rv.len());

        println!("Scenario 4: Not enough arguments");
        listener.handle_abort(&[json!({"x": 1, "y": true})]);
        assert_eq!(0, rv.len());

        println!("Scenario 5: Too many arguments");
        listener.handle_abort(&[
            json!({"x": 1, "y": true}),
            json!("org.foo.bar.error"),
            json!(12345),
        ]);
        assert_eq!(0, rv.len());
    }

    #[test]
    fn handle_goodbye_test() {
        let rv = ReceivedValues::default();
        let (_sender, stop_receiver) = oneshot::channel();
        let mut listener = WebsocketTransportListener {
            stream: unsafe { ::std::mem::uninitialized() },
            received_values: rv.clone(),
            stop_receiver,
        };

        println!("Scenario 0: happy path");
        listener.handle_goodbye(&[json!({"x": 1, "y": true}), json!("org.foo.bar.closed")]);
        assert_eq!(1, rv.goodbye.lock().len());
        assert_eq!(1, rv.len());
        let query = poll_fn(|| {
            let val = try_ready!({
                let res: Result<_, &'static str> = Ok(rv.goodbye.lock().poll_take(|_| true));
                res
            });
            if 2 != val.details.len() {
                return Err(format!("details dict {:?} did not match", val.details));
            }
            if "org.foo.bar.closed" != val.reason.to_raw() {
                return Err(format!("reason URI {:?} did not match", val.reason));
            }
            Ok(Async::Ready(()))
        });
        assert_eq!(current_thread::block_on_all(query), Ok(()));

        println!("Scenario 1: 'Details' is not a dictionary");
        listener.handle_goodbye(&[json!("foobar"), json!("org.foo.bar.closed")]);
        assert_eq!(0, rv.len());

        println!("Scenario 2: 'Reason' is not a string");
        listener.handle_goodbye(&[json!({"x": 1, "y": true}), json!(12345)]);
        assert_eq!(0, rv.len());

        println!("Scenario 3: 'Reason' is not a valid URI");
        listener.handle_goodbye(&[json!({"x": 1, "y": true}), json!("..")]);
        assert_eq!(0, rv.len());

        println!("Scenario 4: Not enough arguments");
        listener.handle_goodbye(&[json!({"x": 1, "y": true})]);
        assert_eq!(0, rv.len());

        println!("Scenario 5: Too many arguments");
        listener.handle_goodbye(&[
            json!({"x": 1, "y": true}),
            json!("org.foo.bar.closed"),
            json!(12345),
        ]);
        assert_eq!(0, rv.len());
    }

    #[cfg(feature = "subscriber")]
    #[test]
    fn handle_subscribed_test() {
        let rv = ReceivedValues::default();
        let (_sender, receiver) = oneshot::channel();
        let mut listener = WebsocketTransportListener {
            stream: unsafe { ::std::mem::uninitialized() },
            received_values: rv.clone(),
            stop_receiver: receiver,
        };

        println!("Scenario 0: happy path");
        listener.handle_subscribed(&[json!(12345), json!(23456)]);
        assert_eq!(1, rv.subscribed.lock().len());
        assert_eq!(1, rv.len());
        let query = poll_fn(|| {
            let val = try_ready!({
                let res: Result<_, &'static str> = Ok(rv.subscribed.lock().poll_take(|_| true));
                res
            });
            if Id::<SessionScope>::from_raw_value(12345) != val.request {
                return Err(format!("request ID {:?} did not match", val.request));
            }
            if Id::<RouterScope>::from_raw_value(23456) != val.subscription {
                return Err(format!(
                    "subscription ID {:?} did not match",
                    val.subscription
                ));
            }
            Ok(Async::Ready(()))
        });
        assert_eq!(current_thread::block_on_all(query), Ok(()));

        println!("Scenario 1: 'Request' is not a number");
        listener.handle_subscribed(&[json!("foobar"), json!(23456)]);
        assert_eq!(0, rv.len());

        println!("Scenario 2: 'Subscription' is not a number");
        listener.handle_subscribed(&[json!(12345), json!("foobar")]);
        assert_eq!(0, rv.len());

        println!("Scenario 3: Not enough arguments");
        listener.handle_subscribed(&[json!(12345)]);
        assert_eq!(0, rv.len());

        println!("Scenario 4: Too many arguments");
        listener.handle_subscribed(&[json!(12345), json!(23456), json!(34567)]);
        assert_eq!(0, rv.len());
    }

    #[test]
    fn handle_unsubscribed_test() {
        let rv = ReceivedValues::default();
        let (_sender, receiver) = oneshot::channel();
        let mut listener = WebsocketTransportListener {
            stream: unsafe { ::std::mem::uninitialized() },
            received_values: rv.clone(),
            stop_receiver: receiver,
        };

        println!("Scenario 0: happy path");
        listener.handle_unsubscribed(&[json!(12345)]);
        assert_eq!(1, rv.unsubscribed.lock().len());
        assert_eq!(1, rv.len());
        let query = poll_fn(|| {
            let val = try_ready!({
                let res: Result<_, &'static str> = Ok(rv.unsubscribed.lock().poll_take(|_| true));
                res
            });
            if Id::<SessionScope>::from_raw_value(12345) != val.request {
                return Err(format!("request ID {:?} did not match", val.request));
            }
            Ok(Async::Ready(()))
        });
        assert_eq!(current_thread::block_on_all(query), Ok(()));

        println!("Scenario 1: request is not a number");
        listener.handle_unsubscribed(&[json!("foobar")]);
        assert_eq!(0, rv.len());

        println!("Scenario 2: Not enough arguments");
        listener.handle_unsubscribed(&[]);
        assert_eq!(0, rv.len());

        println!("Scenario 3: Too many arguments");
        listener.handle_unsubscribed(&[json!(12345), json!(23456)]);
        assert_eq!(0, rv.len());
    }

    #[test]
    fn handle_event_test() {
        let rv = ReceivedValues::default();
        let (_sender, receiver) = oneshot::channel();
        let mut listener = WebsocketTransportListener {
            stream: unsafe { ::std::mem::uninitialized() },
            received_values: rv.clone(),
            stop_receiver: receiver,
        };

        println!("Scenario 0: happy path (no arguments)");
        listener.handle_event(&[json!(12345), json!(23456), json!({"x": "foo", "y": true})]);
        assert_eq!(1, rv.event.lock().len());
        assert_eq!(1, rv.len());
        let query = poll_fn(|| {
            let val = try_ready!({
                let res: Result<_, &'static str> = Ok(rv.event.lock().poll_take(|_| true));
                res
            });
            if Id::<RouterScope>::from_raw_value(12345) != val.subscription {
                return Err(format!(
                    "subscription ID {:?} did not match",
                    val.subscription
                ));
            }
            if Id::<GlobalScope>::from_raw_value(23456) != val.publication {
                return Err(format!(
                    "publication ID {:?} did not match",
                    val.publication
                ));
            }
            if 2 != val.details.len() {
                return Err(format!(
                    "details map {:?} had wrong number of elements",
                    val.details
                ));
            }
            if val.arguments.is_some() {
                return Err(format!("arguments were present: {:?}", val.arguments));
            }
            if val.arguments_kw.is_some() {
                return Err(format!("arguments_kw were present: {:?}", val.arguments_kw));
            }
            Ok(Async::Ready(()))
        });
        assert_eq!(current_thread::block_on_all(query), Ok(()));

        println!("Scenario 1: happy path (positional arguments)");
        listener.handle_event(&[
            json!(12345),
            json!(23456),
            json!({"x": "foo", "y": true}),
            json!([1, "foobar"]),
        ]);
        assert_eq!(1, rv.event.lock().len());
        assert_eq!(1, rv.len());
        let query = poll_fn(|| {
            let val = try_ready!({
                let res: Result<_, &'static str> = Ok(rv.event.lock().poll_take(|_| true));
                res
            });
            if Id::<RouterScope>::from_raw_value(12345) != val.subscription {
                return Err(format!(
                    "subscription ID {:?} did not match",
                    val.subscription
                ));
            }
            if Id::<GlobalScope>::from_raw_value(23456) != val.publication {
                return Err(format!(
                    "publication ID {:?} did not match",
                    val.publication
                ));
            }
            if 2 != val.details.len() {
                return Err(format!(
                    "details map {:?} had wrong number of elements",
                    val.details
                ));
            }
            if let Some(arr) = val.arguments {
                if arr.len() != 2 {
                    return Err(format!("arguments had wrong length {:?}", arr));
                }
                if arr[0] != TransportableValue::Integer(1)
                    || arr[1] != TransportableValue::String("foobar".into())
                {
                    return Err(format!("arguments had wrong values {:?}", arr));
                }
            } else {
                return Err(format!("arguments did not exist"));
            }
            if val.arguments_kw.is_some() {
                return Err(format!("arguments_kw were present: {:?}", val.arguments_kw));
            }
            Ok(Async::Ready(()))
        });
        assert_eq!(current_thread::block_on_all(query), Ok(()));

        println!("Scenario 2: happy path (named arguments)");
        listener.handle_event(&[
            json!(12345),
            json!(23456),
            json!({"x": "foo", "y": true}),
            json!([1, "foobar"]),
            json!({"a": 1, "b": 2}),
        ]);
        assert_eq!(1, rv.event.lock().len());
        assert_eq!(1, rv.len());
        let query = poll_fn(|| {
            let val = try_ready!({
                let res: Result<_, &'static str> = Ok(rv.event.lock().poll_take(|_| true));
                res
            });
            if Id::<RouterScope>::from_raw_value(12345) != val.subscription {
                return Err(format!(
                    "subscription ID {:?} did not match",
                    val.subscription
                ));
            }
            if Id::<GlobalScope>::from_raw_value(23456) != val.publication {
                return Err(format!(
                    "publication ID {:?} did not match",
                    val.publication
                ));
            }
            if 2 != val.details.len() {
                return Err(format!(
                    "details map {:?} had wrong number of elements",
                    val.details
                ));
            }
            if let Some(arr) = val.arguments {
                if arr.len() != 2 {
                    return Err(format!("arguments had wrong length {:?}", arr));
                }
                if arr[0] != TransportableValue::Integer(1)
                    || arr[1] != TransportableValue::String("foobar".into())
                {
                    return Err(format!("arguments had wrong values {:?}", arr));
                }
            } else {
                return Err(format!("arguments did not exist"));
            }
            if let Some(dict) = val.arguments_kw {
                if dict.len() != 2 {
                    return Err(format!("arguments_kw had wrong size"));
                }
                if dict.get("a") != Some(&TransportableValue::Integer(1)) {
                    return Err(format!("arguments_kw had wrong value for a"));
                }
                if dict.get("b") != Some(&TransportableValue::Integer(2)) {
                    return Err(format!("arguments_kw had wrong value for b"));
                }
            }
            Ok(Async::Ready(()))
        });
        assert_eq!(current_thread::block_on_all(query), Ok(()));

        println!("Scenario 3: subscription is not a number");
        listener.handle_event(&[json!("foobar"), json!(23456), json!({})]);
        assert_eq!(0, rv.len());

        println!("Scenario 4: publication is not a number");
        listener.handle_event(&[json!(12345), json!("foobar"), json!({})]);
        assert_eq!(0, rv.len());

        println!("Scenario 5: details is not a dict");
        listener.handle_event(&[json!(12345), json!(23456), json!("foobar")]);
        assert_eq!(0, rv.len());

        println!("Scenario 6: arguments is not a list");
        listener.handle_event(&[json!(12345), json!(23456), json!({}), json!("not a list")]);
        assert_eq!(0, rv.len());

        println!("Scenario 7: arguments_kw is not a list");
        listener.handle_event(&[
            json!(12345),
            json!(23456),
            json!({}),
            json!([]),
            json!("not a dict"),
        ]);
        assert_eq!(0, rv.len());

        println!("Scenario 8: too many arguments");
        listener.handle_event(&[
            json!(12345),
            json!(23456),
            json!({}),
            json!([]),
            json!({}),
            json!("foobar"),
        ]);
        assert_eq!(0, rv.len());

        println!("Scenario 9: not enough arguments");
        listener.handle_event(&[json!(12345), json!(23456)]);
        assert_eq!(0, rv.len());
    }

    #[test]
    fn handle_message_test() {
        let rv = ReceivedValues::default();
        let (_sender, stop_receiver) = oneshot::channel();
        let mut listener = WebsocketTransportListener {
            stream: unsafe { ::std::mem::uninitialized() },
            received_values: rv.clone(),
            stop_receiver,
        };

        println!("Scenario 0: received 'Welcome'");
        listener.handle_message(r#"[2,12345,{"x":1,"y":true}]"#.to_string());
        assert_eq!(1, rv.welcome.lock().len());
        assert_eq!(1, rv.len());
        let query = poll_fn(|| {
            let val = try_ready!({
                let res: Result<_, &'static str> = Ok(rv.welcome.lock().poll_take(|_| true));
                res
            });
            if Id::<GlobalScope>::from_raw_value(12345) != val.session {
                return Err(format!("session id {:?} did not match", val.session));
            }
            if 2 != val.details.len() {
                return Err(format!("details dict {:?} did not match", val.details));
            }
            Ok(Async::Ready(()))
        });
        assert_eq!(current_thread::block_on_all(query), Ok(()));

        println!("Scenario 1: received 'Abort'");
        listener.handle_message(r#"[3,{"x":1,"y":true},"org.foo.bar.error"]"#.to_string());
        assert_eq!(1, rv.abort.lock().len());
        assert_eq!(1, rv.len());
        let query = poll_fn(|| {
            let val = try_ready!({
                let res: Result<_, &'static str> = Ok(rv.abort.lock().poll_take(|_| true));
                res
            });
            if 2 != val.details.len() {
                return Err(format!("details dict {:?} did not match", val.details));
            }
            if "org.foo.bar.error" != val.reason.to_raw() {
                return Err(format!("reason URI {:?} did not match", val.reason));
            }
            Ok(Async::Ready(()))
        });
        assert_eq!(current_thread::block_on_all(query), Ok(()));

        println!("Scenario 2: received 'Goodbye'");
        listener.handle_message(r#"[6,{"x":1,"y":true},"org.foo.bar.closed"]"#.to_string());
        assert_eq!(1, rv.goodbye.lock().len());
        assert_eq!(1, rv.len());
        let query = poll_fn(|| {
            let val = try_ready!({
                let res: Result<_, &'static str> = Ok(rv.goodbye.lock().poll_take(|_| true));
                res
            });
            if 2 != val.details.len() {
                return Err(format!("details dict {:?} did not match", val.details));
            }
            if "org.foo.bar.closed" != val.reason.to_raw() {
                return Err(format!("reason URI {:?} did not match", val.reason));
            }
            Ok(Async::Ready(()))
        });
        assert_eq!(current_thread::block_on_all(query), Ok(()));

        println!("Scenario 3: received non-json text");
        listener.handle_message(r#"~!@#$%^&*()"#.to_string());
        assert_eq!(0, rv.len());

        println!("Scenario 4: received non-array JSON");
        listener.handle_message(r#"{"x":1,"y":true}"#.to_string());
        assert_eq!(0, rv.len());

        println!("Scenario 5: received non-number message code");
        listener.handle_message(r#"["foobar",12345,23456]"#.to_string());
        assert_eq!(0, rv.len());

        println!("Scenario 6: received unknown message code");
        listener.handle_message(r#"[123456,"L"]"#.to_string());
        assert_eq!(0, rv.len());
    }
}
