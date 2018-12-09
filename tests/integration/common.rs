//! Helpers and common code for integration tests.
//! This assumes several things about the current environment:
//!
//! * `crossbar` is installed and available on the `PATH`.

use std::fmt::Debug;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::prelude::*;
use uuid::prelude::*;

pub const TEST_REALM: &str = "wamp_proto_test";

/// Run a future to completion, waiting for the entire runtime to finish, and asserts that it passed.
pub fn assert_future_passes<F, I, E>(future: F)
where
    F: Future<Item = I, Error = E> + Send + 'static,
    E: Debug,
{
    let test_passed = Arc::new(Mutex::new(false));
    let passed_clone = test_passed.clone();

    tokio::run(future.map(move |_| {
        *passed_clone.lock() = true;
    }).map_err(move |e| {
        println!("Future failed: {:?}", e);
    }));

    assert!(*test_passed.lock());
}

/// A handle to a started router; drop to close the router and delete it's config dir.
pub struct RouterHandle {
    config_handle: ConfigHandle,
    router: Child,
}
impl Drop for RouterHandle {
    fn drop(&mut self) {
        self.router.kill().expect("could not kill router")
    }
}

/// Starts a WAMP router, listening on localhost:9001.
pub fn start_router() -> RouterHandle {
    let config_handle = set_crossbar_configuration();
    println!("Created crossbar config: {:?}", config_handle);

    let mut router = Command::new("crossbar")
        .arg("start")
        .arg("--cbdir")
        .arg({
            let mut path = PathBuf::new();
            path.push(&config_handle.path);
            path.push(".crossbar");
            path
        })
        // Tell python (crossbar) to flush stdout after every line
        .env("PYTHONUNBUFFERED", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("could not run `crossbar start`");

    println!("Spawned child process: {}", router.id());

    {
        // Wait for the router to be ready.
        let stdout = BufReader::new(router.stdout.as_mut().expect("did not capture child process stdout"));
        for line in stdout.lines() {
            if line.expect("failed to read child process stdout").contains("Ok, local node configuration booted successfully!") {
                break;
            }
        }
    }

    println!("Crossbar router ready!");
    RouterHandle {
        config_handle,
        router,
    }
}

#[derive(Debug)]
struct ConfigHandle {
    path: PathBuf,
}
impl Drop for ConfigHandle {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path)
            .expect(&format!("failed to remove crossbar config dir {:?}", self.path));
    }
}

fn set_crossbar_configuration() -> ConfigHandle {
    let crossbar_dir = {
        let mut path = PathBuf::new();
        path.push(".");
        path.push(format!("{}", Uuid::new_v4()));
        path
    };

    let status = Command::new("crossbar")
        .arg("init")
        .arg("--appdir")
        .arg(&crossbar_dir)
        .stdout(Stdio::null())
        .status()
        .expect("could not run `crossbar init`");

    if !status.success() {
        panic!("`crossbar init` exited with status code {}", status);
    }

    // Write the configuration...
    let config = json!({
        "$schema": "https://raw.githubusercontent.com/crossbario/crossbar/master/crossbar.json",
        "version": 2,
        "controller": {},
        "workers": [
            {
                "type": "router",
                "realms": [
                    {
                        "name": TEST_REALM,
                        "roles": [
                            {
                                "name": "anonymous",
                                "permissions": [
                                    {
                                        "uri": "",
                                        "match": "prefix",
                                        "allow": {
                                            "call": true,
                                            "register": true,
                                            "publish": true,
                                            "subscribe": true,
                                        },
                                        "disclose": {
                                            "caller": true,
                                            "publisher": true,
                                        },
                                        "cache": true,
                                    }
                                ]
                            }
                        ]
                    }
                ],
                "transports": [
                    {
                        "type": "websocket",
                        "endpoint": {
                            "type": "tcp",
                            "port": 9001,
                        },
                        "debug": true,
                    }
                ]
            }
        ]
    });

    {
        let mut path = PathBuf::new();
        path.push(&crossbar_dir);
        path.push(".crossbar");
        path.push("config.json");

        let file = File::create(path)
            .expect("could not open crossbar config file");
        serde_json::to_writer_pretty(&file, &config)
            .expect("could not write to crossbar config file");
    }

    ConfigHandle {
        path: fs::canonicalize(crossbar_dir).expect("failed to canonicalize crossbar dir")
    }
}