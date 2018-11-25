
use std::collections::HashMap;

use failure::Error;
use futures::{
    Async,
    AsyncSink,
    Future,
    sink::Sink,
    stream::{Stream, SplitSink, SplitStream},
};
use serde_json::{self, Value};
use tokio::reactor;
use websocket::{
    ClientBuilder,
    async::{
        TcpStream,
        client::{Client, ClientNew},
    },
    message::{Message, OwnedMessage},
};

use proto::{
    rx::{self, RxMessage},
    TxMessage,
};
use {
    ConnectResult, GlobalScope, Id, ReceivedValues, RouterScope, SessionScope,
    Transport, TransportableValue, Uri
};

/// An implementation of a websocket-based WAMP Transport.
pub struct WebsocketTransport {
    client: SplitSink<Client<TcpStream>>,

    // Only present until listen() has been called once
    stream: Option<SplitStream<Client<TcpStream>>>,
    received_values: Option<ReceivedValues>,
}
impl Sink for WebsocketTransport {
    type SinkItem = TxMessage;
    type SinkError = Error;

    fn start_send(&mut self, item: Self::SinkItem) -> Result<AsyncSink<Self::SinkItem>, Self::SinkError> {
        let value = item.to_json();
        self.client.start_send(Message::text(serde_json::to_string(&value)?).into()).map(|async| {
            match async {
                AsyncSink::NotReady(_) => AsyncSink::NotReady(item),
                AsyncSink::Ready => AsyncSink::Ready,
            }
        }).map_err(|e| e.into())
    }

    fn poll_complete(&mut self) -> Result<Async<()>, Self::SinkError> {
        self.client.poll_complete().map_err(|e| e.into())
    }
}
impl Transport for WebsocketTransport {
    type ConnectFuture = WebsocketTransportConnectFuture;

    fn connect(url: &str) -> Result<ConnectResult<Self>, Error> {
        let client_future = ClientBuilder::new(url)?.async_connect_insecure(&reactor::Handle::current());
        let received_values = ReceivedValues::default();

        Ok(ConnectResult {
            future: WebsocketTransportConnectFuture {
                future: client_future,
                received_values: Some(received_values.clone()),
            },
            received_values,
        })
    }

    fn listen(&mut self) {
        if let (Some(stream), Some(received_values)) = (self.stream.take(), self.received_values.take()) {
            tokio::spawn(WebsocketTransportListener { stream, received_values });
        } else {
            warn!("WebsocketTransport::listen called multiple times!");
        }
    }
}

struct WebsocketTransportListener {
    stream: SplitStream<Client<TcpStream>>,
    received_values: ReceivedValues,
}
impl Future for WebsocketTransportListener {
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Result<Async<Self::Item>, Self::Error> {
        self.poll_impl().map_err(|e| {
            error!("{:?}", e);
            ()
        })
    }
}
impl WebsocketTransportListener {
    fn poll_impl(&mut self) -> Result<Async<()>, Error> {
        loop {
            match self.stream.poll()? {
                // Happy path
                Async::Ready(Some(OwnedMessage::Text(message))) => self.handle_message(message),

                // Received some non-text message: log and move on
                Async::Ready(Some(message)) => {
                    warn!("received unknown message {:?}", message);
                }

                // Received no message: stream is closed
                Async::Ready(None) => {
                    info!("Websocket underlying stream closed!");
                    return Ok(Async::Ready(()));
                }

                // Nothing available
                Async::NotReady => return Ok(Async::NotReady),
            }
        }
    }

    fn handle_message(&mut self, message: String) {
        if let Ok(Value::Array(vals)) = serde_json::from_str(&message) {
            trace!("Received websocket message: {:?}", vals);
            if vals.len() > 0 {
                if let Some(code) = vals[0].as_u64() {
                    match code {
                        rx::Welcome::MSG_CODE => self.handle_welcome(&vals[1..]),
                        rx::Abort::MSG_CODE => self.handle_abort(&vals[1..]),
                        rx::Goodbye::MSG_CODE => self.handle_goodbye(&vals[1..]),
                        rx::Subscribed::MSG_CODE => self.handle_subscribed(&vals[1..]),

                        _ => {
                            warn!("received unknown message code {}", code);
                        }
                    }
                } else {
                    warn!("received non-integer message code {:?}", vals[0]);
                }
            } else {
                warn!("received zero-length message");
            }
        } else {
            warn!("received bad or non-array JSON {}", message);
        }
    }

    fn handle_welcome(&mut self, msg: &[Value]) {
        if msg.len() != 2 {
            warn!("bad WELCOME message length");
            return;
        }

        let session: Id<GlobalScope>;
        let details: HashMap<String, TransportableValue>;

        if let Some(session_id) = msg[0].as_u64() {
            session = Id::<GlobalScope>::from_raw_value(session_id);
        } else {
            warn!("bad WELCOME message session ID {:?}", msg[0]);
            return;
        }

        if let Some(TransportableValue::Dict(details_)) = json_to_tv(&msg[1]) {
            details = details_;
        } else {
            warn!("bad WELCOME message details {:?}", msg[1]);
            return;
        }

        trace!("Adding WELCOME message: {:?}, {:?}", session, details);
        self.received_values.welcome.lock().insert(rx::Welcome { session, details });
    }

    fn handle_abort(&mut self, msg: &[Value]) {
        if msg.len() != 2 {
            warn!("bad ABORT message length");
            return;
        }

        let details: HashMap<String, TransportableValue>;
        let reason: Uri;

        if let Some(TransportableValue::Dict(details_)) = json_to_tv(&msg[0]) {
            details = details_;
        } else {
            warn!("bad ABORT message details {:?}", msg[0]);
            return;
        }

        if let Some(uri_str) = msg[1].as_str() {
            if let Some(reason_) = Uri::relaxed(uri_str) {
                reason = reason_;
            } else {
                warn!("bad URI in ABORT message reason {:?}", msg[1]);
                return;
            }
        } else {
            warn!("bad ABORT message reason {:?}", msg[1]);
            return;
        }

        trace!("Adding ABORT message: {:?} {:?}", details, reason);
        self.received_values.abort.lock().insert(rx::Abort { details, reason });
    }

    fn handle_goodbye(&mut self, msg: &[Value]) {
        if msg.len() != 2 {
            warn!("bad GOODBYE message length");
            return;
        }

        let details: HashMap<String, TransportableValue>;
        let reason: Uri;

        if let Some(TransportableValue::Dict(details_)) = json_to_tv(&msg[0]) {
            details = details_;
        } else {
            warn!("bad GOODBYE message details {:?}", msg[0]);
            return;
        }

        if let Some(uri_str) = msg[1].as_str() {
            if let Some(reason_) = Uri::relaxed(uri_str) {
                reason = reason_;
            } else {
                warn!("bad URI in GOODBYE message reason {:?}", msg[1]);
                return;
            }
        } else {
            warn!("bad GOODBYE message reason {:?}", msg[1]);
            return;
        }

        trace!("Adding GOODBYE message: {:?} {:?}", details, reason);
        self.received_values.goodbye.lock().insert(rx::Goodbye { details, reason });
    }

    fn handle_subscribed(&mut self, msg: &[Value]) {
        if msg.len() != 2 {
            warn!("bad SUBSCRIBED message length");
            return;
        }

        let request: Id<SessionScope>;
        let subscription: Id<RouterScope>;

        if let Some(request_id_raw) = msg[0].as_u64() {
            request = Id::<SessionScope>::from_raw_value(request_id_raw);
        } else {
            warn!("bad SUBSCRIBED message request ID {:?}", msg[0]);
            return;
        }

        if let Some(subscription_id_raw) = msg[1].as_u64() {
            subscription = Id::<RouterScope>::from_raw_value(subscription_id_raw);
        } else {
            warn!("bad SUBSCRIBED message subscription ID {:?}", msg[1]);
            return;
        }

        trace!("Adding SUBSCRIBED message: {:?} {:?}", request, subscription);
        self.received_values.subscribed.lock().insert(rx::Subscribed { request, subscription });
    }
}

fn json_to_tv(value: &Value) -> Option<TransportableValue> {
    Some(match value {
        Value::Null => return None,
        Value::Bool(val) => TransportableValue::Bool(*val),
        Value::Number(num) => if let Some(val) = num.as_u64() {
            TransportableValue::Integer(val)
        } else {
            warn!("skipping negative or floating point number {:?}", num);
            return None
        }
        Value::String(val) => TransportableValue::String(val.clone()),
        Value::Array(vals) => TransportableValue::List(
            vals.into_iter().filter_map(json_to_tv).collect()
        ),
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

/// Returned by [`WebsocketTransport::Connect`]; resolves to a [`WebsocketTransport`].
pub struct WebsocketTransportConnectFuture {
    future: ClientNew<TcpStream>,
    received_values: Option<ReceivedValues>,
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
                    client,
                    stream: Some(stream),
                    received_values: Some(self.received_values
                        .take()
                        .expect("invalid WebsocketTransportConnectFuture state")
                    ),
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future::poll_fn;
    use tokio::runtime::current_thread;

    use std::io::{self, prelude::*};
    use std::sync::Arc;
    use parking_lot::Mutex;

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
        assert_eq!(
            Some(TransportableValue::Integer(4)),
            json_to_tv(&json!(4))
        );
        assert_eq!(
            Some(TransportableValue::Integer(0)),
            json_to_tv(&json!(0))
        );
        assert_eq!(
            Some(TransportableValue::Integer(0x7FFF_FFFF_FFFF_FFFF)),
            json_to_tv(&json!(0x7FFF_FFFF_FFFF_FFFFu64))
        );
        assert_eq!(
            None,
            json_to_tv(&json!(-1))
        );
        assert_eq!(
            None,
            json_to_tv(&json!(-3))
        );
    }

    #[test]
    fn handle_welcome_test() {
        let rv = ReceivedValues::default();
        let mut listener = WebsocketTransportListener {
            stream: unsafe { ::std::mem::uninitialized() },
            received_values: rv.clone(),
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
                return Err(format!("session id {:?} did not match", val.session))
            }
            if 2 != val.details.len() {
                return Err(format!("details dict {:?} did not match", val.details))
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
        let mut listener = WebsocketTransportListener {
            stream: unsafe { ::std::mem::uninitialized() },
            received_values: rv.clone(),
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
            if "org.foo.bar.error" != val.reason.0 {
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
        listener.handle_abort(&[json!({"x": 1, "y": true}), json!("org.foo.bar.error"), json!(12345)]);
        assert_eq!(0, rv.len());
    }

    #[test]
    fn handle_goodbye_test() {
        let rv = ReceivedValues::default();
        let mut listener = WebsocketTransportListener {
            stream: unsafe { ::std::mem::uninitialized() },
            received_values: rv.clone(),
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
            if "org.foo.bar.closed" != val.reason.0 {
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
        listener.handle_goodbye(&[json!({"x": 1, "y": true}), json!("org.foo.bar.closed"), json!(12345)]);
        assert_eq!(0, rv.len());
    }

    #[cfg(feature = "subscriber")]
    #[test]
    fn handle_subscribed_test() {
        let rv = ReceivedValues::default();
        let mut listener = WebsocketTransportListener {
            stream: unsafe { ::std::mem::uninitialized() },
            received_values: rv.clone(),
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
                return Err(format!("subscription ID {:?} did not match", val.subscription));
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
    fn handle_message_test() {
        let rv = ReceivedValues::default();
        let mut listener = WebsocketTransportListener {
            stream: unsafe { ::std::mem::uninitialized() },
            received_values: rv.clone(),
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
                return Err(format!("session id {:?} did not match", val.session))
            }
            if 2 != val.details.len() {
                return Err(format!("details dict {:?} did not match", val.details))
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
            if "org.foo.bar.error" != val.reason.0 {
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
            if "org.foo.bar.closed" != val.reason.0 {
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