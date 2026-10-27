//! The sync service's file send: `SEND path,mode`, `DATA` chunks of at most
//! 64 KiB, `DONE mtime`, then `OKAY` or `FAIL msg`. Every header is a 4-byte
//! id and a u32 LE length (or value).

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use retv_bridge_core::api::TransportError;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

use crate::errors::stream_error;
use crate::host::{read_exact_idle, write_all_idle, AdbHost};

const MAX_CHUNK: usize = 64 * 1024;
const MAX_FAIL_MESSAGE: usize = 64 * 1024;
/// `S_IFREG`: adbd expects a full `st_mode`.
const S_IFREG: u32 = 0o100000;

/// Why a push failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PushError {
    /// The adb server or the device connection failed, or the reply was
    /// malformed (`Internal`).
    Transport(TransportError),
    /// The device refused the file (`FAIL msg`, e.g. a read-only path).
    Failed(String),
}

impl From<TransportError> for PushError {
    fn from(e: TransportError) -> Self {
        PushError::Transport(e)
    }
}

impl fmt::Display for PushError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PushError::Transport(e) => e.fmt(f),
            PushError::Failed(msg) => write!(f, "push failed: {msg}"),
        }
    }
}

impl std::error::Error for PushError {}

fn header(id: &[u8; 4], value: u32) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[..4].copy_from_slice(id);
    out[4..].copy_from_slice(&value.to_le_bytes());
    out
}

pub(crate) async fn send(
    host: &AdbHost,
    mut sock: TcpStream,
    path: &str,
    bytes: &[u8],
    mode: u32,
) -> Result<(), PushError> {
    let idle = host.timeouts.idle;
    let spec = format!("{path},{}", S_IFREG | (mode & 0o7777));
    let mut out = header(b"SEND", spec.len() as u32).to_vec();
    out.extend_from_slice(spec.as_bytes());
    let mut written = write_all_idle(&mut sock, &out, idle).await;
    if written.is_ok() {
        for chunk in bytes.chunks(MAX_CHUNK) {
            let mut out = header(b"DATA", chunk.len() as u32).to_vec();
            out.extend_from_slice(chunk);
            written = write_all_idle(&mut sock, &out, idle).await;
            if written.is_err() {
                break;
            }
        }
    }
    if written.is_ok() {
        let mtime = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as u32)
            .unwrap_or(0);
        written = write_all_idle(&mut sock, &header(b"DONE", mtime), idle).await;
    }
    if let Err(err) = written {
        // adbd may have refused the file early (FAIL) and stopped reading:
        // that reply explains the stalled or broken write.
        return match read_reply(host, &mut sock).await {
            Err(PushError::Failed(msg)) => Err(PushError::Failed(msg)),
            _ => Err(PushError::Transport(stream_error(err))),
        };
    }
    read_reply(host, &mut sock).await?;
    let _ = sock.write_all(&header(b"QUIT", 0)).await;
    Ok(())
}

async fn read_reply(host: &AdbHost, sock: &mut TcpStream) -> Result<(), PushError> {
    let idle = host.timeouts.idle;
    let mut reply = [0u8; 8];
    read_exact_idle(sock, &mut reply, idle)
        .await
        .map_err(stream_error)?;
    let len = u32::from_le_bytes([reply[4], reply[5], reply[6], reply[7]]) as usize;
    match &reply[..4] {
        b"OKAY" => Ok(()),
        b"FAIL" if len > MAX_FAIL_MESSAGE => Err(PushError::Transport(host.internal(format!(
            "sync FAIL message of {len} bytes is over the {MAX_FAIL_MESSAGE}-byte cap"
        )))),
        b"FAIL" => {
            let mut msg = vec![0u8; len];
            read_exact_idle(sock, &mut msg, idle)
                .await
                .map_err(stream_error)?;
            Err(PushError::Failed(
                String::from_utf8_lossy(&msg).into_owned(),
            ))
        }
        _ => Err(PushError::Transport(TransportError::ConnectionLost)),
    }
}
