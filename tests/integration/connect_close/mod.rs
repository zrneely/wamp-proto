//! Coordinates running the basic "can I connect to a router" test.

use std::sync::Arc;

use integration::common::*;

use parking_lot::Mutex;
use tokio::prelude::*;
use wamp_proto::{transport::websocket::WebsocketTransport, Client, ClientConfig, Uri};

lazy_static! {
    static ref SAVED_CLIENT: Arc<Mutex<Option<Client<WebsocketTransport>>>> = Arc::new(Mutex::new(None));
}

#[test]
fn connect_close() {
    let _router = start_router();

    let client_config = ClientConfig::new(TEST_URI, Uri::strict(TEST_REALM).unwrap());
    let future = Client::<WebsocketTransport>::new(client_config);

    assert_future_passes(10, future.and_then(|client| {
        *SAVED_CLIENT.lock() = Some(client);

        SAVED_CLIENT.lock().as_mut().unwrap().close(Uri::strict("wamp.error.goodbye").unwrap())
    }));
}

#[test]
fn connect_then_router_closed() {
    let router = start_router();

    let client_config = ClientConfig::new(TEST_URI, Uri::strict(TEST_REALM).unwrap());
    let future = Client::<WebsocketTransport>::new(client_config);

    assert_future_passes(10, future.map_err(|_| "connect error").and_then(move |client| {
        *SAVED_CLIENT.lock() = Some(client);

        // Stop the router and wait for the client to stop itself
        drop(router);

        future::poll_fn(|| -> Poll<(), &'static str> {
            if SAVED_CLIENT.lock().as_ref().unwrap().is_open() {
                Ok(Async::NotReady)
            } else {
                Ok(Async::Ready(()))
            }
        })
    }));
}