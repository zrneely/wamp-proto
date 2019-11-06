//! Basic "can I connect to a router" tests.

use crate::integration::common::*;

use wamp_proto::{transport::websocket::WebsocketTransport, uri::Uri, Client, ClientConfig};

#[tokio::test]
async fn connect_close() {
    let router = start_router().await;
    let url = router.get_url();

    let client_config = ClientConfig::new(&url, Uri::strict(TEST_REALM).unwrap());
    let mut client = Client::<WebsocketTransport>::new(client_config)
        .await
        .unwrap();

    client
        .close(Uri::strict("wamp.error.goodbye").unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn connect_then_router_closed() {
    let router = start_router().await;
    let url = router.get_url();

    let client_config = ClientConfig::new(&url, Uri::strict(TEST_REALM).unwrap());
    let client = Client::<WebsocketTransport>::new(client_config)
        .await
        .unwrap();

    drop(router);

    client.wait_for_close().await;
}
