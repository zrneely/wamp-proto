use wamp_proto::{
    transport::{websocket::WebsocketTransport, TransportableValue as TV},
    uri::Uri,
    Broadcast, Client, ClientConfig,
};

use std::collections::HashMap;
use std::time::Duration;

use futures::stream::StreamExt as _;
use tokio::future::FutureExt as _;

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
        .subscribe(&Uri::strict("org.test.channel").unwrap())
        .await
        .unwrap();

    let subscription_future = test_channel_subscription.for_each(|broadcast| {
        async move {
            println!("Got broadcast: {:?}", broadcast);
        }
    });
    tokio::spawn(subscription_future);

    client
        .publish(
            &Uri::strict("org.test.channel").unwrap(),
            Broadcast {
                arguments: vec![TV::Integer(1)],
                arguments_kw: HashMap::default(),
            },
        )
        .await
        .unwrap();

    client
        .wait_for_close()
        .timeout(Duration::from_secs(60 * 60))
        .await
        .unwrap();
}
