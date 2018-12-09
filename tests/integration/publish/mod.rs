//! Publisher tests.

use std::sync::Arc;

use integration::common::*;

use parking_lot::Mutex;
use tokio::prelude::*;
use wamp_proto::{transport::websocket::WebsocketTransport, Client, ClientConfig, Uri};

lazy_static! {
    static ref SAVED_CLIENT: Arc<Mutex<Option<Client<WebsocketTransport>>>> = Arc::new(Mutex::new(None));
}

#[test]
fn publish_one_message() {
    let _router = start_router();

    let client_config = ClientConfig::new(TEST_URI, Uri::strict(TEST_REALM).unwrap());
    let future = Client::<WebsocketTransport>::new(client_config);

    assert_future_passes(10, future.and_then(|client| {
        *SAVED_CLIENT.lock() = Some(client);

        SAVED_CLIENT.lock().as_mut().unwrap().close(Uri::strict("wamp.error.goodbye").unwrap())
    }));
}