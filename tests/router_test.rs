extern crate futures;
#[macro_use]
extern crate lazy_static;
extern crate env_logger;
extern crate tokio;
extern crate wamp_proto;

use tokio::prelude::*;
use wamp_proto::{transport::websocket::WebsocketTransport, Client, ClientConfig, Uri};

use std::sync::{Arc, Mutex};
use std::time::Duration;

lazy_static! {
    static ref SAVED_CLIENT: Arc<Mutex<Option<Client<WebsocketTransport>>>> =
        Arc::new(Mutex::new(None));
}

#[test]
fn integration_1() {
    env_logger::init();

    let mut client_config =
        ClientConfig::new("ws://127.0.0.1:9001", Uri::strict("org.test").unwrap());
    // TODO: clean shutdown on timeout or network error from transport
    client_config.timeout = Duration::from_secs(60 * 10);
    client_config.shutdown_timeout = Duration::from_secs(60 * 10);
    client_config.user_agent = Some("WampProto Test".into());

    let future = Client::<WebsocketTransport>::new(client_config)
        .and_then(|mut client| {
            let future =
                client.subscribe(Uri::strict("org.test.channel").unwrap(), move |broadcast| {
                    println!(
                        "got broadcast: {:?}, {:?}",
                        broadcast.arguments, broadcast.arguments_kw
                    );
                    Box::new(futures::future::ok(()))
                });

            *SAVED_CLIENT.lock().unwrap() = Some(client);
            future

            // If we don't close ourselves and the router closes the connection, all tasks should
            // terminate gracefully and this program should finish. Comment the and_then and map calls
            // to simulate that.

            }).and_then(|subscription| {
                println!("got subscription {:?}", subscription);
                // println!("closing client");
                // SAVED_CLIENT.lock().unwrap().as_mut().unwrap().close(Uri::raw("wamp.error.goodbye".to_string()))
                
                println!("Unsubscribing");
                SAVED_CLIENT.lock().unwrap().as_mut().unwrap().unsubscribe(subscription)
            }).and_then(|_| {
                println!("unsubscribed! closing client");
                SAVED_CLIENT.lock().unwrap().as_mut().unwrap().close(Uri::raw("wamp.error.goodbye".to_string()))
            }).map(|_| {
                println!("client closed!");

                // Comment these two lines to simulate not dropping the client
                SAVED_CLIENT.lock().unwrap().take();
                println!("client dropped!");

        }).map_err(|e| println!("error: {:?}", e))
        .map(|_| ());

    tokio::run(future);
}
