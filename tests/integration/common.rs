//! Helpers and common code for integration tests.
//! This assumes several things about the current environment:
//!
//! * `crossbar` is installed and available on the `PATH`.
//! * `nodejs` is installed and available on the `PATH`.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;

use futures::{pin_mut, stream::StreamExt as _};
use tokio::{
    codec::{FramedRead, LinesCodec, LinesCodecError},
    net::process,
    prelude::*,
};
use uuid::prelude::*;

pub const TEST_REALM: &str = "wamp_proto_test";

pub struct PeerHandle {
    _peer: process::Child,
    stdout: Pin<Box<dyn Stream<Item = Result<String, LinesCodecError>>>>,
    panic_on_drop: bool,
}
impl PeerHandle {
    pub async fn wait_for_test_complete(mut self) -> Result<(), ()> {
        self.panic_on_drop = false;

        tokio::future::poll_fn(|cx| match self.stdout.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(line))) => {
                if line.contains("test passed") {
                    Poll::Ready(Ok(()))
                } else if line.contains("test failed") {
                    Poll::Ready(Err(()))
                } else {
                    Poll::Pending
                }
            }
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(Err(())),
            Poll::Ready(Some(Err(_))) => Poll::Ready(Err(())),
        })
        .await
    }
}

/// Starts the current test module's peer.
pub async fn start_peer<T: AsRef<Path>>(
    module: T,
    test: &str,
    router: &RouterHandle,
) -> PeerHandle {
    let mut peer = process::Command::new("python3")
        .arg({
            let mut path = PathBuf::new();
            path.push(".");
            path.push("tests");
            path.push("integration");
            path.push(module.as_ref());
            path.push("peer.py");
            path
        })
        .arg(test)
        .arg(router.get_url())
        .arg(TEST_REALM)
        // Tell python to flush stdout after every line
        .env("PYTHONUNBUFFERED", "1")
        // Tell python to use UTF-8 for I/O
        .env("PYTHONIOENCODING", "utf8")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("could not start python");

    // Wait for the peer to signal that it's ready.
    {
        let stdout = FramedRead::new(
            peer.stdout().as_mut().unwrap(),
            LinesCodec::new_with_max_length(1024),
        )
        .inspect(|line| {
            if let Ok(line) = line {
                println!("Peer stdout: {}", line);
            }
        });

        // Wait for the peer to signal that it's ready.
        let filtered_stdout = stdout.filter_map(|line| {
            async move {
                if let Ok(line) = line {
                    if line.contains("ready") {
                        return Some(line);
                    }
                }
                None
            }
        });

        pin_mut!(filtered_stdout);
        filtered_stdout.next().await;
    }

    let stdout = Box::pin(
        FramedRead::new(
            peer.stdout().take().unwrap(),
            LinesCodec::new_with_max_length(1024),
        )
        .inspect(|line| {
            if let Ok(line) = line {
                println!("Peer stdout: {}", line);
            }
        }),
    );

    PeerHandle {
        _peer: peer,
        stdout,
        panic_on_drop: true,
    }
}

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
        .arg("--loglevel")
        .arg("trace")
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
    {
        let stdout = FramedRead::new(
            router.stdout().as_mut().unwrap(),
            LinesCodec::new_with_max_length(1024),
        )
        .inspect(|line| {
            if let Ok(line) = line {
                println!("Router stdout: {}", line);
            }
        });

        // Wait for the peer to signal that it's ready.
        let filtered_stdout = stdout.filter_map(|line| {
            async move {
                if let Ok(line) = line {
                    if line.contains("Ok, local node configuration booted successfully!") {
                        return Some(line);
                    }
                }
                None
            }
        });

        pin_mut!(filtered_stdout);
        filtered_stdout.next().await;
    }

    tokio::spawn(
        FramedRead::new(
            router.stdout().take().unwrap(),
            LinesCodec::new_with_max_length(1024),
        )
        .map(|line| {
            if let Ok(line) = line {
                println!("Router stdout: {}", line);
            }
        })
        .collect::<Vec<_>>()
        .map(|_| ()),
    );

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
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
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
