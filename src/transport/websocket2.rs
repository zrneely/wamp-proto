use std::pin::Pin;
use std::task::{Context, Poll};

use async_trait::async_trait;
use failure::{Error, Fail};
use futures::stream::{SplitSink, SplitStream};
use tokio::prelude::*;
use tokio_net::tcp::TcpStream;
use tokio_tungstenite::{
    tungstenite::{self, handshake::client::Request, protocol::Message},
    MaybeTlsStream, WebSocketStream,
};
use url::Url;

use crate::{
    proto::{rx, TxMessage},
    transport::Transport,
};

#[derive(Debug, Fail)]
enum WSError {
    #[fail(display = "{}", 0)]
    Tungstenite(tungstenite::Error),
}
impl From<tungstenite::Error> for WSError {
    fn from(err: tungstenite::Error) -> Self {
        WSError::Tungstenite(err)
    }
}

/// A Transport implementation that works over websockets.
pub struct WebsocketTransport {}

#[async_trait]
impl Transport for WebsocketTransport {
    type Sink = WampSinkAdapter;
    type Stream = WampStreamAdapter;

    async fn connect(url: &str) -> Result<(WampSinkAdapter, WampStreamAdapter), Error> {
        let url = Url::parse(url)?;
        let request = {
            let mut request = Request {
                url,
                extra_headers: None,
            };
            request.add_protocol("wamp.2.json".into());
            request
        };

        let (wss, _) = tokio_tungstenite::connect_async(request).await?;
        let (sink, stream) = wss.split();

        Ok((WampSinkAdapter { sink }, WampStreamAdapter { stream }))
    }
}

/// A sink for TxMessages.
pub struct WampSinkAdapter {
    sink: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
}
impl Sink<TxMessage> for WampSinkAdapter {
    type Error = Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Error>> {
        Pin::new(&mut self.sink)
            .poll_ready(cx)
            .map_err(|err| err.into())
    }

    fn start_send(mut self: Pin<&mut Self>, item: TxMessage) -> Result<(), Error> {
        let message = Message::text(serde_json::to_string(&item.to_json())?);
        Pin::new(&mut self.sink)
            .start_send(message)
            .map_err(|err| err.into())
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Error>> {
        Pin::new(&mut self.sink)
            .poll_flush(cx)
            .map_err(|err| err.into())
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Error>> {
        Pin::new(&mut self.sink)
            .poll_close(cx)
            .map_err(|err| err.into())
    }
}

/// A stream of RxMessages.
pub struct WampStreamAdapter {
    stream: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
}
impl Stream for WampStreamAdapter {
    type Item = rx::RxMessage;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<rx::RxMessage>> {
        unimplemented!()
    }
}
