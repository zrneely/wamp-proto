
use failure::Error;
use futures::{
    Async,
    AsyncSink,
    Future,
    sink::Sink,
};
use serde_json;
use tokio_core::reactor;
use websocket::{
    ClientBuilder,
    async::{
        TcpStream,
        client::{Client, ClientNew},
    },
};

use proto::{msg_code, TxMessage};
use {ConnectResult, ReceivedValues, Transport};

/// An implementation of a websocket-based WAMP Transport.
pub struct WebsocketTransport {
    client: Client<TcpStream>,
    received_values: ReceivedValues,
}
impl Sink for WebsocketTransport {
    type SinkItem = TxMessage;
    type SinkError = Error;

    fn start_send(&mut self, item: Self::SinkItem) -> Result<AsyncSink<Self::SinkItem>, Self::SinkError> {
        let value: serde_json::Value = match item {
            TxMessage::Hello { realm, details } => json!([
                msg_code::HELLO,
                realm,
                {},
            ]),
            _ => unimplemented!()
        };
        // TODO transform the item into JSON text and send it over the inner sink as a text message
        unimplemented!()
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

        // TODO spawn task to receive values from the client

        Ok(ConnectResult {
            future: WebsocketTransportConnectFuture { client_future, rv: Some(received_values.clone()) },
            received_values,
        })
    }
}

pub struct WebsocketTransportConnectFuture {
    client_future: ClientNew<TcpStream>,
    rv: Option<ReceivedValues>,
}
impl Future for WebsocketTransportConnectFuture {
    type Item = WebsocketTransport;
    type Error = Error;

    fn poll(&mut self) -> Result<Async<Self::Item>, Self::Error> {
        // If the client is ready, make a transport
        match self.client_future.poll()? {
            Async::NotReady => return Ok(Async::NotReady),
            Async::Ready((client, _headers)) => {
                return Ok(Async::Ready(WebsocketTransport {
                    client,
                    received_values: self.rv.take().expect("bad WTCF state"),
                }))
            }
        }
    }
}