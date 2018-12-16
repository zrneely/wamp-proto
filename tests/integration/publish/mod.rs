//! Publisher tests.

use std::collections::HashMap;
use std::sync::Arc;

use integration::common::*;

use parking_lot::Mutex;
use tokio::prelude::*;
use wamp_proto::{transport::websocket::WebsocketTransport, Broadcast, Client, ClientConfig, TransportableValue, Uri};

lazy_static! {
    static ref SAVED_CLIENT: Arc<Mutex<Option<Client<WebsocketTransport>>>> = Arc::new(Mutex::new(None));
}

#[test]
fn publish_one_message() {
    let router = start_router();
    let url = router.get_url();
    let client_config = ClientConfig::new(&url, Uri::strict(TEST_REALM).unwrap());
    let future = Client::<WebsocketTransport>::new(client_config);

    assert_future_passes_and_peer_ok(10, start_peer("publish", "publishOneMessage", &router), future.and_then(|client| {
        println!("client, router, and peer ready");
        *SAVED_CLIENT.lock() = Some(client);
        SAVED_CLIENT.lock().as_mut().unwrap().publish(Uri::strict("org.test.topic1").unwrap(), Broadcast {
            arguments: vec![TransportableValue::Integer(42)],
            arguments_kw: HashMap::new(),
        })
    }).and_then(|_| {
        println!("published; closing client");
        SAVED_CLIENT.lock().as_mut().unwrap().close(Uri::strict("wamp.error.goodbye").unwrap())
    }));

    println!("dropping router");
}