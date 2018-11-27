
extern crate futures;
#[macro_use]
extern crate lazy_static;
extern crate tokio;
extern crate wamp_proto;
extern crate env_logger;

use tokio::prelude::*;
use wamp_proto::{Client, ClientConfig, Uri, transport::websocket::WebsocketTransport};

use std::sync::{Arc, Mutex};
use std::time::Duration;

lazy_static! {
    static ref SAVED_CLIENT: Arc<Mutex<Option<Client<WebsocketTransport>>>> = Arc::new(Mutex::new(None));
}

#[test]
fn integration_1() {
    env_logger::init();

    let mut client_config = ClientConfig::new(
        "ws://127.0.0.1:9001",
        Uri::strict("org.test").unwrap()
    );
    client_config.timeout = Duration::from_secs(60 * 10);
    client_config.shutdown_timeout = Duration::from_secs(60 * 10);
    client_config.panic_on_drop_while_open = false;

    let future = Client::<WebsocketTransport>::new(client_config)
        .and_then(|mut client| {
            let future = client.subscribe(Uri::strict("org.test.channel").unwrap(), move |broadcast| {
                println!("got broadcast: {:?}, {:?}", broadcast.arguments, broadcast.arguments_kw);
                Box::new(futures::future::ok(()))
            });

            *SAVED_CLIENT.lock().unwrap() = Some(client);
            future
        }).and_then(|subscription| {
            println!("got subscription {:?}", subscription);
            println!("closing client");
            SAVED_CLIENT.lock().unwrap().as_mut().unwrap().close(Uri::raw("wamp.error.goodbye".to_string()))
        }).map(|_| {
            println!("client closed!");
            SAVED_CLIENT.lock().unwrap().take();
            println!("client dropped!");
            ()
        }).map_err(|e| { panic!("error: {:?}", e) });

    tokio::run(future);
}