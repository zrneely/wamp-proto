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

    let client_config = ClientConfig::new("ws://127.0.0.1:9001", Uri::strict(TEST_REALM).unwrap());
    let future = Client::<WebsocketTransport>::new(client_config);

    assert_future_passes(future.and_then(|client| {
        *SAVED_CLIENT.lock() = Some(client);

        SAVED_CLIENT.lock().as_mut().unwrap().close(Uri::strict("wamp.error.goodbye").unwrap())
    }));
}