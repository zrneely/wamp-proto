//! Helpers and common code for integration tests.
//! This assumes several things about the current environment:
//!
//! * `crossbar` is installed and available on the `PATH`.
//! * `nodejs` is installed and available on the `PATH`.

use std::fmt::Debug;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use std::time::Duration;

use parking_lot::Mutex;
use tokio::prelude::*;
use tokio_process::CommandExt;
use uuid::prelude::*;

pub const TEST_REALM: &str = "wamp_proto_test";

/// Run a future to completion, waiting for the entire runtime to finish, and asserts that it passed.
pub fn assert_future_passes<F, E>(timeout_secs: u64, future: F)
where
    F: Future<Item = (), Error = E> + Send + 'static,
    E: Debug,
{
    let test_passed = Arc::new(Mutex::new(false));
    let passed_clone = test_passed.clone();

    tokio::run(future
        .timeout(Duration::from_secs(timeout_secs))
        .map(move |_| {
            *passed_clone.lock() = true;
        }).map_err(move |e| {
            println!("Future failed: {:?}", e);
        })
    );

    assert!(*test_passed.lock());
}

pub fn assert_future_passes_and_peer_ok<P, F, E>(timeout_secs: u64, peer_future: P, test_future: F)
where
    P: Future<Item = PeerHandle, Error = String> + Send + 'static,
    F: Future<Item = (), Error = E> + Send + 'static,
    E: Debug,
{
    println!("building peer-then-argument future");
    assert_future_passes(
        timeout_secs,
        peer_future.and_then(|mut peer| test_future
            .map_err(|e| format!("{:?}", e))
            .join(future::poll_fn(move || {
                loop {
                    match peer.stdout.poll() {
                        Ok(Async::Ready(Some(line))) => {
                            println!("from peer: {}", line);
                            if line.contains("test passed") {
                                return Ok(Async::Ready(()));
                            } else if line.contains("test failed") {
                                return Err("peer explicitly failed test!".into());
                            }
                            // ignore all lines that don't contain "test passed" or "test failed"
                        },
                        Ok(Async::Ready(None)) => return Err("peer stdout closed before passing/failing test".into()),
                        Ok(Async::NotReady) => return Ok(Async::NotReady),
                        Err(e) => return Err(format!("{:?}", e)),
                    }
                }
            }))
        ).map(|_| ())
    );
}

pub struct PeerHandle {
    _peer: tokio_process::Child,
    pub stdout: Box<dyn Stream<Item = String, Error = tokio::io::Error> + Send>,
}

/// Starts the current test module's peer.
pub fn start_peer<T: AsRef<Path>>(module: T, test: &str, router: &RouterHandle) -> impl Future<Item = PeerHandle, Error = String> + Send + 'static {
    let mut peer = Command::new("python3")
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
        .stdout(Stdio::piped())
        .spawn_async()
        .expect("could not start `python3.6`");

    let stdout = BufReader::new(peer.stdout().take().expect("did not capture child process stdout"));
    let mut handle = Some(PeerHandle {
        _peer: peer,
        stdout: Box::new(tokio::io::lines(stdout)),
    });

    future::poll_fn(move || {
        // Wait for the peer to be ready.
        loop {
            match handle.as_mut().unwrap().stdout.poll() {
                Ok(Async::Ready(Some(line))) => {
                    println!("from peer: {}", line);
                    if line.contains("ready") {
                        println!("peer ready");
                        return Ok(Async::Ready(handle.take().unwrap()));
                    }
                    // ignore all lines that don't contain "ready"
                },
                Ok(Async::Ready(None)) => return Err("peer stdout closed before ready".into()),
                Ok(Async::NotReady) => return Ok(Async::NotReady),
                Err(e) => return Err(format!("{:?}", e)),
            }
        }
    })
}

/// A handle to a started router; drop to close the router and delete it's config dir.
pub struct RouterHandle {
    crossbar_dir: PathBuf,
    router: std::process::Child,
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
        fs::remove_dir_all(&self.crossbar_dir)
            .expect(&format!("failed to remove crossbar config dir {:?}", self.crossbar_dir));
    }
}

/// Starts a WAMP router, listening on localhost:9001.
pub fn start_router() -> RouterHandle {
    lazy_static! {
        static ref PORT_NUMBER: AtomicUsize = AtomicUsize::new(9000);
    }
    let port = PORT_NUMBER.fetch_add(1, Ordering::SeqCst) as u16;
    let crossbar_dir = set_crossbar_configuration(port);
    println!("Created crossbar config: {:?}", crossbar_dir);

    let mut router = Command::new("crossbar")
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
        crossbar_dir,
        router,
        port,
    }
}

fn set_crossbar_configuration(port: u16) -> PathBuf {
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

        let file = File::create(path)
            .expect("could not open crossbar config file");
        serde_json::to_writer_pretty(&file, &config)
            .expect("could not write to crossbar config file");
    }

    fs::canonicalize(crossbar_dir).expect("failed to canonicalize crossbar dir")
}