use std::fs::{self, File};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::{io::BufReader, prelude::*, process};
use uuid::prelude::*;

use crate::integration::common::TEST_REALM;

/// A handle to a started router; drop to close the router and delete its config dir.
pub struct RouterHandle {
    crossbar_dir: PathBuf,
    router: process::Child,
    port: u16,
}
impl RouterHandle {
    /// Gets the URL to connect to the router.
    pub fn get_url(&self) -> String {
        format!("ws://127.0.0.1:{}", self.port)
    }
}
impl Drop for RouterHandle {
    fn drop(&mut self) {
        self.router.kill().expect("could not kill router");
        if let Err(err) = fs::remove_dir_all(&self.crossbar_dir) {
            println!("Failed to delete temp dir {:?}: {}", self.crossbar_dir, err);
        }
    }
}

/// Starts a WAMP router, listening on localhost:9001.
pub async fn start_router() -> RouterHandle {
    lazy_static! {
        static ref PORT_NUMBER: AtomicUsize = AtomicUsize::new(9000);
    }
    let port = PORT_NUMBER.fetch_add(1, Ordering::SeqCst) as u16;
    let crossbar_dir = set_crossbar_configuration(port).await;
    println!("Created crossbar config: {:?}", crossbar_dir);

    let mut router = process::Command::new("crossbar")
        .arg("start")
        .arg("--cbdir")
        .arg({
            let mut path = PathBuf::new();
            path.push(&crossbar_dir);
            path.push(".crossbar");
            path
        })
        // Tell python (crossbar) to flush stdout after every line
        .env("PYTHONUNBUFFERED", "1")
        // Tell python (crossbar) to use UTF-8 for I/O
        .env("PYTHONIOENCODING", "utf8")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("could not run crossbar");

    println!("Spawned child process: {}", router.id());

    // Wait for the router to be ready.
    // Wait for the peer to signal that it's ready.
    let mut stdout = BufReader::new(router.stdout.as_mut().unwrap()).lines();

    loop {
        match stdout.next_line().await {
            Ok(Some(line))
                if line.contains("Ok, local node configuration booted successfully!") =>
            {
                break;
            }
            _ => {}
        }
    }

    println!("Crossbar router ready!");
    RouterHandle {
        crossbar_dir,
        router,
        port,
    }
}

async fn set_crossbar_configuration(port: u16) -> PathBuf {
    let crossbar_dir = {
        let mut path = tempfile::tempdir().unwrap().into_path();
        path.push(".");
        path.push(format!("{}", Uuid::new_v4()));
        path
    };

    let status = process::Command::new("crossbar")
        .arg("init")
        .arg("--appdir")
        .arg(&crossbar_dir)
        .stdout(Stdio::null())
        .status()
        .await
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
                            "port": port,
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

        let file = File::create(path).expect("could not open crossbar config file");
        serde_json::to_writer_pretty(&file, &config)
            .expect("could not write to crossbar config file");
    }

    fs::canonicalize(crossbar_dir).expect("failed to canonicalize crossbar dir")
}
