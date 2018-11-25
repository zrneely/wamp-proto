
extern crate futures;
extern crate tokio;
extern crate wamp_proto;
extern crate env_logger;

use tokio::prelude::*;
use wamp_proto::{Client, Uri, transport};

use std::time::Duration;

#[test]
fn integration_1() {
    env_logger::init();

    let future = Client::<transport::websocket::WebsocketTransport>::new(
        "ws://127.0.0.1:9001",
        Uri::strict("org.test").unwrap(),
        Duration::from_secs(10 * 60 * 60)
    ).unwrap().and_then(|client| {
        println!("got client! {:#?}", client);

        // client.subscribe(Uri::strict("org.test.channel").unwrap(), move |broadcast| {
        //     println!("got broadcast: {:?}, {:?}", broadcast.arguments, broadcast.arguments_kw);
        //     Box::new(futures::future::ok(()))
        // }).unwrap()
        future::ok(true)
    }).map(|v| {
        println!("subscribed! result: {:?}", v);
        ()
    }).map_err(|e| { panic!("error: {:?}", e); () });

    tokio::run(future);
}