//! Subscription tests

use crate::integration::common::*;

use futures::stream::StreamExt;
use wamp_proto::{transport::websocket::WebsocketTransport, uri::Uri, Client, ClientConfig};

#[tokio::test]
async fn subscribe_unsubscribe() {
    let router = start_router().await;
    let url = router.get_url();

    let client_config = ClientConfig::new(&url, Uri::strict(TEST_REALM).unwrap());
    let mut client = Client::<WebsocketTransport>::new(client_config)
        .await
        .unwrap();

    let subscription = client
        .subscribe(Uri::strict("org.test.channel").unwrap())
        .await
        .unwrap();

    client.unsubscribe(subscription).await.unwrap();

    client
        .close(Uri::strict("wamp.error.goodbye").unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn subscribe_recv_unsubscribe() {
    let router = start_router().await;
    let url = router.get_url();

    let client_config = ClientConfig::new(&url, Uri::strict(TEST_REALM).unwrap());
    let mut client = Client::<WebsocketTransport>::new(client_config)
        .await
        .unwrap();

    println!("client and router ready");

    let mut subscription = client
        .subscribe(Uri::strict("org.test.channel").unwrap())
        .await
        .unwrap();

    let peer = start_peer("subscribe", "subscribeRecvUnsubscribe", &router).await;
    println!("peer sent message");

    let message = subscription.next().await.unwrap();
    assert_eq!(message.arguments.len(), 1);
    let message_arg_0 = message.arguments[0].clone().into_dict().unwrap();
    assert_eq!(message_arg_0.get("a").unwrap().as_int().unwrap(), 1);
    assert_eq!(message_arg_0.get("b").unwrap().as_bool().unwrap(), false);
    let list = message_arg_0.get("c").unwrap().clone().into_list().unwrap();
    assert_eq!(list.len(), 3);
    assert_eq!(list[0].as_int().unwrap(), 3);
    assert_eq!(list[1].as_int().unwrap(), 4);
    assert_eq!(list[2].as_int().unwrap(), 5);

    client.unsubscribe(subscription).await.unwrap();

    client
        .close(wamp_proto::uri::known_uri::session_close::system_shutdown)
        .await
        .unwrap();

    peer.wait_for_test_complete().await.unwrap();
}
