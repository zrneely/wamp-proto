//! Publisher tests.

use std::collections::HashMap;

use crate::integration::common::*;

use wamp_proto::{
    transport::{websocket::WebsocketTransport, TransportableValue as TV},
    uri::Uri,
    Broadcast, Client, ClientConfig,
};

#[tokio::test]
async fn publish_one_message() {
    let router = start_router().await;
    let url = router.get_url();

    let peer = start_peer("publish", "publishOneMessage", &router).await;

    let client_config = ClientConfig::new(&url, Uri::strict(TEST_REALM).unwrap());
    let mut client = Client::<WebsocketTransport>::new(client_config)
        .await
        .unwrap();

    println!("client, router, and peer ready");

    client
        .publish(
            &Uri::strict("org.test.topic1").unwrap(),
            Broadcast {
                arguments: vec![TV::Integer(42)],
                arguments_kw: HashMap::new(),
            },
        )
        .await
        .unwrap();

    client
        .close(&wamp_proto::uri::known_uri::session_close::system_shutdown)
        .await
        .unwrap();

    peer.wait_for_test_complete().await.unwrap();
}
