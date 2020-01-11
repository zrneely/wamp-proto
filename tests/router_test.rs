use wamp_proto::{transport::websocket::WebsocketTransport, uri::Uri, Client, ClientConfig};

use std::time::Duration;

use futures::stream::StreamExt as _;
use tokio::time::timeout;

#[tokio::test]
#[ignore]
async fn integration_1() {
    env_logger::init();

    let mut client_config =
        ClientConfig::new("ws://127.0.0.1:9001", Uri::strict("org.test").unwrap());
    client_config.user_agent = Some("WampProto Test".into());

    let mut client = Client::<WebsocketTransport>::new(client_config)
        .await
        .unwrap();

    let test_channel_subscription = client
        .subscribe(Uri::strict("org.test.channel").unwrap())
        .await
        .unwrap();

    let subscription_future = test_channel_subscription.for_each(|broadcast| {
        async move {
            println!("Got broadcast: {:?}", broadcast);
        }
    });
    tokio::spawn(subscription_future);

    timeout(Duration::from_secs(60 * 60), client.wait_for_close())
        .await
        .unwrap();
}
