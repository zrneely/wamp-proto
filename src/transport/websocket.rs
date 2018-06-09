
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
use tokio_core::reactor;
use websocket::{
    ClientBuilder,
    async::{
        TcpStream,
        client::{Client, ClientNew},
    },
    message::{Message, OwnedMessage},
};

use proto::{
    msg_code,
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
        // TODO: avoid clone here, somehow?
        let value: serde_json::Value = match &item {
            &TxMessage::Hello { ref realm, details: _ } => json!([
                msg_code::HELLO,
                realm.clone(),
                {},
            ]),
            &TxMessage::Goodbye { ref details, ref reason } => json!([
                msg_code::GOODBYE,
                details,
                reason.clone(),
            ]),
            _ => unimplemented!()
        };
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

    fn connect(url: &str, handle: &reactor::Handle) -> Result<ConnectResult<Self>, Error> {
        let client_future = ClientBuilder::new(url)?.async_connect_insecure(handle);
        let received_values = ReceivedValues::default();

        Ok(ConnectResult {
            future: WebsocketTransportConnectFuture {
                future: client_future,
                received_values: Some(received_values.clone()),
            },
            received_values,
        })
    }

    fn listen(&mut self, handle: &reactor::Handle) {
        if let (Some(stream), Some(received_values)) = (self.stream.take(), self.received_values.take()) {
            handle.spawn(WebsocketTransportListener { stream, received_values });
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
            reason = Uri::raw(uri_str.into());
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
            reason = Uri::raw(uri_str.into());
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

    // TODO test all the handle_* methods

    #[test]
    fn handle_welcome_test() {
        let mut core = ::tokio_core::reactor::Core::new().unwrap();

        let rv = ReceivedValues::default();
        let mut listener = WebsocketTransportListener {
            stream: unsafe { ::std::mem::uninitialized() },
            received_values: rv.clone(),
        };

        // TODO negative test cases

        listener.handle_welcome(&[json!(12345), json!({"x": 1, "y": true})]);
        assert_eq!(1, rv.welcome.lock().len());
        let query = future::poll_fn(|| {
            let val = try_ready!({
                let res: Result<_, &'static str> = Ok(rv.welcome.lock().poll_take(|_| true));
                res
            });
            if Id::<GlobalScope>::from_raw_value(12345) != val.session {
                return Err("session id did not match")
            }
            if 2 != val.details.len() {
                return Err("details dict did not match")
            }
            Ok(Async::Ready(()))
        });
        assert!(core.run(query).is_ok());
    }
}