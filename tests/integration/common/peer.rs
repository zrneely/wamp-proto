use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::{io::BufReader, prelude::*, process};

use crate::integration::common::{router::RouterHandle, TEST_REALM};

struct Tee<T: AsyncRead + Unpin> {
    name: &'static str,
    inner: tokio::io::Lines<BufReader<T>>,
}
impl<T: AsyncRead + Unpin> Tee<T> {
    async fn next_line(&mut self) -> tokio::io::Result<Option<String>> {
        match self.inner.next_line().await {
            Ok(Some(text)) => {
                println!("{}: {}", self.name, text);
                Ok(Some(text))
            }
            t => t,
        }
    }
}

pub struct PeerHandle {
    stdout: Tee<process::ChildStdout>,
    stderr: Tee<process::ChildStderr>,
    panic_on_drop: bool,
}
impl PeerHandle {
    pub async fn wait_for_test_complete(mut self) -> Result<(), ()> {
        self.panic_on_drop = false;

        loop {
            tokio::select! {
                line = self.stdout.next_line() => {
                    match line {
                        Ok(Some(line)) if line.contains("test passed") => {
                            return Ok(());
                        }
                        Ok(Some(line)) if line.contains("test failed") => {
                            return Err(());
                        }
                        Ok(Some(_)) => {}
                        Ok(None) => return Err(()),
                        Err(_) => return Err(()),
                    }
                }

                line = self.stderr.next_line() => {
                    match line {
                        Ok(Some(_)) => {
                            println!("Detected stderr message, failing test");
                            return Err(());
                        }
                        Ok(None) => return Err(()),
                        Err(_) => return Err(()),
                    }
                }
            };
        }
    }
}

/// Starts the current test module's peer.
pub async fn start_peer<T: AsRef<Path>>(
    module: T,
    test: &str,
    router: &RouterHandle,
) -> PeerHandle {
    let mut peer = process::Command::new("python")
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
        .stderr(Stdio::piped())
        .spawn()
        .expect("could not start python");

    println!("Started peer python");

    // Wait for the peer to signal that it's ready.
    let mut stdout = Tee {
        inner: BufReader::new(peer.stdout.take().unwrap()).lines(),
        name: "peer stdout",
    };

    let stderr = Tee {
        inner: BufReader::new(peer.stderr.take().unwrap()).lines(),
        name: "peer stderr",
    };

    // We need an additional task to drive the subprocess to completion.
    tokio::spawn(async {
        let status = peer.await.expect("could not start python");
        println!("Peer exited with status {}", status);
        if !status.success() {
            panic!("Peer exited with non-zero status");
        }
    });

    loop {
        if let Ok(Some(line)) = stdout.next_line().await {
            if line.contains("ready") {
                break;
            }
        }
    }

    println!("Peer ready!");
    PeerHandle {
        stdout,
        stderr,
        panic_on_drop: true,
    }
}
