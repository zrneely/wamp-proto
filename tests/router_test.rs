extern crate futures;
#[macro_use]
extern crate lazy_static;
extern crate env_logger;
extern crate tokio;
extern crate wamp_proto;

use wamp_proto::{transport::websocket::WebsocketTransport, uri::Uri, Client, ClientConfig};

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::stream::StreamExt as _;
use tokio::future::FutureExt as _;

lazy_static! {
    static ref SAVED_CLIENT: Arc<Mutex<Option<Client<WebsocketTransport>>>> =
        Arc::new(Mutex::new(None));
}

#[tokio::test]
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

    client
        .wait_for_close()
        .timeout(Duration::from_secs(60 * 60))
        .await
        .unwrap();
}
