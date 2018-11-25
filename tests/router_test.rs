
extern crate futures;
extern crate tokio;
extern crate wamp_proto;
extern crate env_logger;

use tokio::prelude::*;
use wamp_proto::{Client, ClientConfig, Uri, transport};

use std::time::Duration;

#[test]
fn integration_1() {
    env_logger::init();

    let client_config = ClientConfig {
        url: "ws://127.0.0.1:9001",
        realm: Uri::strict("org.test").unwrap(),
        timeout: Duration::from_secs(60 * 10),
        shutdown_timeout: Duration::from_secs(1),
    };

    let future = Client::<transport::websocket::WebsocketTransport>::new(client_config)
        .and_then(|mut client| {
            println!("got client! {:#?}", client);

            client.subscribe(Uri::strict("org.test.channel").unwrap(), move |broadcast| {
                println!("got broadcast: {:?}, {:?}", broadcast.arguments, broadcast.arguments_kw);
                Box::new(futures::future::ok(()))
            }).unwrap()
        }).map(|v| {
            println!("subscribed! result: {:?}", v);
            ()
        }).map_err(|e| { panic!("error: {:?}", e) });

    tokio::run(future);
}