// Increase the compile-time recursion limit so that the json!() macro can be fully expanded.
#![recursion_limit="128"]

#[macro_use]
extern crate lazy_static;
extern crate parking_lot;
#[macro_use]
extern crate serde_json;
extern crate tokio;
extern crate tokio_process;
extern crate uuid;
extern crate wamp_proto;

mod integration;