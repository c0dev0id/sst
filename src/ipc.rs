//! Local send socket. The process holding the Signal session (the TUI or
//! `sst watch`) listens on it and `sst send` is its client, so only one process
//! ever writes ratchet state. One request per connection, newline-delimited JSON.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use presage::Manager;
use presage::manager::Registered;
use presage::store::{Store, Thread};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot};

use crate::signal;

#[derive(Serialize, Deserialize)]
pub struct SendRequest {
    pub recipient: String,
    pub text: String,
    pub attachments: Vec<PathBuf>,
}

impl SendRequest {
    /// Validate the recipient and make attachment paths absolute: the session
    /// holder reads them from its own working directory.
    pub fn new(recipient: String, text: String, attachments: Vec<PathBuf>) -> anyhow::Result<Self> {
        signal::parse_thread_id(&recipient)?;
        let attachments = attachments
            .into_iter()
            .map(|p| std::fs::canonicalize(&p).with_context(|| p.display().to_string()))
            .collect::<Result<_, _>>()?;
        Ok(Self { recipient, text, attachments })
    }
}

#[derive(Serialize, Deserialize)]
struct SendResponse {
    ok: bool,
    error: Option<String>,
}

/// A request received over the socket, to be carried out by the session holder.
pub struct SendJob {
    pub request: SendRequest,
    reply: oneshot::Sender<Result<(), String>>,
}

impl SendJob {
    pub fn finish(self, result: anyhow::Result<()>) {
        let _ = self.reply.send(result.map_err(|e| format!("{e:#}")));
    }
}

pub fn socket_path(data_dir: &Path) -> PathBuf {
    data_dir.join("sst.sock")
}

/// Removes the socket file when the session holder exits.
pub struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Listen on the send socket; each request arrives as a SendJob on the returned
/// channel. Must run inside a LocalSet. The caller holds the instance lock, so a
/// socket file left behind by an earlier crash is stale and is replaced.
pub fn serve(data_dir: &Path) -> anyhow::Result<(mpsc::Receiver<SendJob>, SocketGuard)> {
    let path = socket_path(data_dir);
    let listener = match UnixListener::bind(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            std::fs::remove_file(&path)?;
            UnixListener::bind(&path)
        }
        other => other,
    }
    .with_context(|| format!("failed to listen on {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;

    let (jobs, receiver) = mpsc::channel(8);
    tokio::task::spawn_local(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    tokio::task::spawn_local(handle_connection(stream, jobs.clone()));
                }
                Err(e) => {
                    tracing::warn!("send socket accept failed: {e}");
                    break;
                }
            }
        }
    });
    Ok((receiver, SocketGuard(path)))
}

async fn handle_connection(stream: tokio::net::UnixStream, jobs: mpsc::Sender<SendJob>) {
    let (reader, mut writer) = stream.into_split();
    let mut line = String::new();
    if tokio::io::BufReader::new(reader).read_line(&mut line).await.is_err() {
        return;
    }
    let outcome = match serde_json::from_str::<SendRequest>(&line) {
        Ok(request) => {
            let (reply, done) = oneshot::channel();
            match jobs.send(SendJob { request, reply }).await {
                Ok(()) => done.await.unwrap_or_else(|_| Err("session holder went away".to_string())),
                Err(_) => Err("session holder is shutting down".to_string()),
            }
        }
        Err(e) => Err(format!("malformed request: {e}")),
    };
    let response = SendResponse { ok: outcome.is_ok(), error: outcome.err() };
    let mut encoded = serde_json::to_string(&response).expect("response serializes");
    encoded.push('\n');
    let _ = writer.write_all(encoded.as_bytes()).await;
}

/// Hand a request to a running session holder. Ok(None) when none is listening.
pub fn request_send(path: &Path, request: &SendRequest) -> anyhow::Result<Option<Result<(), String>>> {
    use std::io::ErrorKind::{ConnectionRefused, NotFound};
    let mut stream = match UnixStream::connect(path) {
        Ok(stream) => stream,
        Err(e) if matches!(e.kind(), NotFound | ConnectionRefused) => return Ok(None),
        Err(e) => return Err(e).context("send socket"),
    };
    let mut encoded = serde_json::to_string(request)?;
    encoded.push('\n');
    stream.write_all(encoded.as_bytes())?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).context("reading the send socket reply")?;
    let response: SendResponse = serde_json::from_str(&line).context("malformed send socket reply")?;
    Ok(Some(if response.ok {
        Ok(())
    } else {
        Err(response.error.unwrap_or_else(|| "send failed".to_string()))
    }))
}

/// Carry out a request with the session: upload the attachments and send.
/// Returns the thread and the message timestamp.
pub async fn perform<S: Store>(
    manager: &mut Manager<S, Registered>,
    request: &SendRequest,
) -> anyhow::Result<(Thread, u64)> {
    let thread = signal::parse_thread_id(&request.recipient)?;
    let ts = signal::send_text(manager, &thread, &request.text, &request.attachments).await?;
    Ok((thread, ts))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sst-ipc-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn request() -> SendRequest {
        SendRequest {
            recipient: "11111111-1111-1111-1111-111111111111".to_string(),
            text: "hi\nthere".to_string(),
            attachments: vec![PathBuf::from("/tmp/x.png")],
        }
    }

    #[tokio::test]
    async fn no_listener_means_no_holder() {
        let path = scratch_dir("none").join("sst.sock");
        assert!(request_send(&path, &request()).unwrap().is_none());
    }

    #[tokio::test]
    async fn request_and_reply_round_trip_over_the_socket() {
        let dir = scratch_dir("roundtrip");
        tokio::task::LocalSet::new()
            .run_until(async {
                let (mut jobs, _guard) = serve(&dir).unwrap();
                let path = socket_path(&dir);
                // Blocking client off the runtime thread, which must keep driving the server task.
                let client = tokio::task::spawn_blocking(move || request_send(&path, &request()));

                let job = jobs.recv().await.unwrap();
                assert_eq!(job.request.text, "hi\nthere");
                assert_eq!(job.request.attachments, vec![PathBuf::from("/tmp/x.png")]);
                job.finish(Err(anyhow::anyhow!("not a member")));

                let outcome = client.await.unwrap().unwrap();
                assert_eq!(outcome, Some(Err("not a member".to_string())));
            })
            .await;
    }

    #[tokio::test]
    async fn a_stale_socket_file_is_replaced() {
        let dir = scratch_dir("stale");
        std::fs::write(socket_path(&dir), b"").unwrap();
        tokio::task::LocalSet::new()
            .run_until(async {
                let (_jobs, guard) = serve(&dir).unwrap();
                drop(guard);
                assert!(!socket_path(&dir).exists());
            })
            .await;
    }
}
