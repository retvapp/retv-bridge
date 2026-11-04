//! A fake Samsung sdbd: CNXN, shell services with canned output, sync push.
#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Default, Clone)]
pub struct Seen {
    /// Every service opened, in order (`shell:0 getduid`, `sync:`).
    pub services: Vec<String>,
    /// Pushed files: path,mode → bytes.
    pub pushed: Vec<(String, Vec<u8>)>,
}

#[derive(Clone)]
pub enum Mode {
    Normal,
    /// Closes the socket right after the client's CNXN (Host PC IP is another computer).
    CloseAfterCnxn,
    /// Answers a push with FAIL and this text.
    PushFails(String),
    /// Closes the sync stream after DONE without a status.
    CloseAfterDone,
}

pub struct FakeSdbd {
    pub addr: SocketAddr,
    pub seen: Arc<Mutex<Seen>>,
}

fn packet(cmd: &[u8; 4], a0: u32, a1: u32, data: &[u8]) -> Vec<u8> {
    let c = u32::from_le_bytes(*cmd);
    let sum: u32 = data
        .iter()
        .map(|b| *b as u32)
        .fold(0u32, |a, b| a.wrapping_add(b));
    let mut v = Vec::with_capacity(24 + data.len());
    for x in [c, a0, a1, data.len() as u32, sum, c ^ 0xffff_ffff] {
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.extend_from_slice(data);
    v
}

async fn read_packet(s: &mut TcpStream) -> Option<([u8; 4], u32, u32, Vec<u8>)> {
    let mut h = [0u8; 24];
    s.read_exact(&mut h).await.ok()?;
    let cmd: [u8; 4] = h[0..4].try_into().unwrap();
    let a0 = u32::from_le_bytes(h[4..8].try_into().unwrap());
    let a1 = u32::from_le_bytes(h[8..12].try_into().unwrap());
    let n = u32::from_le_bytes(h[12..16].try_into().unwrap()) as usize;
    let magic = u32::from_le_bytes(h[20..24].try_into().unwrap());
    assert_eq!(magic, u32::from_le_bytes(cmd) ^ 0xffff_ffff, "bad magic");
    let mut data = vec![0u8; n];
    s.read_exact(&mut data).await.ok()?;
    Some((cmd, a0, a1, data))
}

impl FakeSdbd {
    /// `outputs`: shell command (after `shell:`) → output.
    pub async fn start(outputs: HashMap<String, String>, mode: Mode) -> FakeSdbd {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let seen2 = seen.clone();
        let outputs = Arc::new(outputs);
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    return;
                };
                let seen = seen2.clone();
                let outputs = outputs.clone();
                let mode = mode.clone();
                tokio::spawn(async move { serve(sock, seen, outputs, mode).await });
            }
        });
        FakeSdbd { addr, seen }
    }
}

async fn serve(
    mut s: TcpStream,
    seen: Arc<Mutex<Seen>>,
    outputs: Arc<HashMap<String, String>>,
    mode: Mode,
) {
    let Some((cmd, _, _, data)) = read_packet(&mut s).await else {
        return;
    };
    assert_eq!(&cmd, b"CNXN");
    assert_eq!(data, b"host::\0");
    if matches!(mode, Mode::CloseAfterCnxn) {
        return;
    }
    s.write_all(&packet(b"CNXN", 0x0200_0000, 256 * 1024, b"device::\0"))
        .await
        .unwrap();
    let mut next_remote = 100u32;
    // Per remote id: sync state.
    let mut sync_buf: HashMap<u32, Vec<u8>> = HashMap::new();
    while let Some((cmd, a0, a1, data)) = read_packet(&mut s).await {
        match &cmd {
            b"OPEN" => {
                let service = String::from_utf8_lossy(&data)
                    .trim_end_matches('\0')
                    .to_string();
                seen.lock().unwrap().services.push(service.clone());
                let local = a0;
                next_remote += 1;
                let remote = next_remote;
                s.write_all(&packet(b"OKAY", remote, local, &[]))
                    .await
                    .unwrap();
                if let Some(shell) = service.strip_prefix("shell:") {
                    let out = outputs.get(shell).cloned().unwrap_or_default();
                    // Two writes, to check the client joins them.
                    let (a, b) = out.as_bytes().split_at(out.len() / 2);
                    for part in [a, b] {
                        if part.is_empty() {
                            continue;
                        }
                        s.write_all(&packet(b"WRTE", remote, local, part))
                            .await
                            .unwrap();
                        let (c, _, _, _) = read_packet(&mut s).await.unwrap();
                        assert_eq!(&c, b"OKAY");
                    }
                    s.write_all(&packet(b"CLSE", remote, local, &[]))
                        .await
                        .unwrap();
                } else if service == "sync:" {
                    sync_buf.insert(remote, Vec::new());
                }
            }
            b"WRTE" => {
                let remote = a1;
                let local = a0;
                s.write_all(&packet(b"OKAY", remote, local, &[]))
                    .await
                    .unwrap();
                let buf = sync_buf
                    .get_mut(&remote)
                    .expect("WRTE to an unknown stream");
                buf.extend_from_slice(&data);
                if let Some((path, body)) = parse_sync(buf) {
                    if matches!(mode, Mode::CloseAfterDone) {
                        s.write_all(&packet(b"CLSE", remote, local, &[]))
                            .await
                            .unwrap();
                        continue;
                    }
                    let status = match &mode {
                        Mode::PushFails(msg) => {
                            let mut v = b"FAIL".to_vec();
                            v.extend_from_slice(&(msg.len() as u32).to_le_bytes());
                            v.extend_from_slice(msg.as_bytes());
                            v
                        }
                        _ => {
                            seen.lock().unwrap().pushed.push((path, body));
                            let mut v = b"OKAY".to_vec();
                            v.extend_from_slice(&0u32.to_le_bytes());
                            v
                        }
                    };
                    s.write_all(&packet(b"WRTE", remote, local, &status))
                        .await
                        .unwrap();
                }
            }
            b"OKAY" => {}
            b"CLSE" => {
                let _ = s.write_all(&packet(b"CLSE", a1, a0, &[])).await;
            }
            other => panic!("unexpected packet {:?}", String::from_utf8_lossy(other)),
        }
    }
}

/// A complete SEND…DONE: (path,mode, data).
fn parse_sync(buf: &[u8]) -> Option<(String, Vec<u8>)> {
    if buf.len() < 8 || &buf[0..4] != b"SEND" {
        return None;
    }
    let n = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as usize;
    let spec = String::from_utf8_lossy(buf.get(8..8 + n)?).to_string();
    let mut i = 8 + n;
    let mut body = Vec::new();
    loop {
        let id = buf.get(i..i + 4)?;
        let len = u32::from_le_bytes(buf.get(i + 4..i + 8)?.try_into().unwrap()) as usize;
        if id == b"DONE" {
            return Some((spec, body));
        }
        assert_eq!(id, b"DATA");
        body.extend_from_slice(buf.get(i + 8..i + 8 + len)?);
        i += 8 + len;
    }
}
