mod parsers;

use std::pin::Pin;
use std::task::{Context, Poll};

use async_trait::async_trait;
use failure::Error;
use futures::{
    sink::Sink,
    stream::{SplitSink, SplitStream, Stream, StreamExt as _},
};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    tungstenite::{handshake::client::Request, protocol::Message},
    WebSocketStream,
};

use crate::{
    proto::{rx::RxMessage, TxMessage},
    transport::{Transport, TransportError},
};

/// A Transport implementation that works over websockets.
pub enum WebsocketTransport {}
#[async_trait]
impl Transport for WebsocketTransport {
    type Sink = WampSinkAdapter;
    type Stream = WampStreamAdapter;

    async fn connect(url: &str) -> Result<(WampSinkAdapter, WampStreamAdapter), Error> {
        let request = Request::builder()
            .header("Sec-Websocket-Protocol", "wamp.2.json")
            .uri(url)
            .body(())?;

        let (wss, _) = tokio_tungstenite::connect_async(request).await?;
        let (sink, stream) = wss.split();

        Ok((WampSinkAdapter { sink }, WampStreamAdapter { stream }))
    }
}

/// A sink for TxMessages.
pub struct WampSinkAdapter {
    sink: SplitSink<WebSocketStream<TcpStream>, Message>,
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
    stream: SplitStream<WebSocketStream<TcpStream>>,
}
impl Stream for WampStreamAdapter {
    type Item = Result<RxMessage, TransportError>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<RxMessage, TransportError>>> {
        loop {
            match Pin::new(&mut self.stream).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Ready(Some(Err(error))) => {
                    return Poll::Ready(Some(Err(TransportError::NetworkError(error.into()))));
                }
                Poll::Ready(Some(Ok(message))) => match parsers::parse_message(message) {
                    Ok(Some(message)) => return Poll::Ready(Some(Ok(message))),
                    Ok(None) => continue,
                    Err(error) => {
                        return Poll::Ready(Some(Err(TransportError::ParseError(error.into()))));
                    }
                },
            }
        }
    }
}
