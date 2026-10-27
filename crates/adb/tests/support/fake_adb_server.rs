//! A scripted stand-in for the adb server: smart-socket framing (4 hex digits
//! of length, then the payload; `OKAY` / `FAIL`), shell v2 packets and the
//! sync `SEND`/`DATA`/`DONE` exchange. Each accepted socket serves one host
//! request, as the real server does.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// One scripted shell command: stdout, stderr and the exit code. `exit: None`
/// sends the stdout and then drops the socket without an exit packet.
/// stdout goes out in packets of at most 16 KiB, stderr after the first one.
#[derive(Clone, Debug, Default)]
pub struct ShellReply {
    pub stdout: String,
    pub stderr: String,
    pub exit: Option<u8>,
    /// Write the reply one byte per write.
    pub trickle: bool,
    /// After the output, keep the socket open and silent (no exit packet).
    pub hang: bool,
    /// Send these bytes verbatim instead of the packets above, then close.
    pub raw: Option<Vec<u8>>,
}

impl ShellReply {
    pub fn new(stdout: &str, stderr: &str, exit: u8) -> Self {
        ShellReply {
            stdout: stdout.into(),
            stderr: stderr.into(),
            exit: Some(exit),
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct FakeAdbScript {
    pub version: u32,
    /// `host:connect:<addr>`: `Ok(serial)` answers `connected to <serial>`
    /// (and the serial becomes `device`), `Err(msg)` answers `msg`.
    pub connect: HashMap<String, Result<String, String>>,
    /// `host:pair:<code>:<addr>`, keyed by `(code, addr)`.
    pub pair: HashMap<(String, String), Result<(), String>>,
    pub shell: HashMap<String, ShellReply>,
    /// When set, every sync `DONE` is answered with `FAIL <msg>`.
    pub sync_fail: Option<String>,
    /// Device states reported by `get-state`; a connected serial defaults to
    /// `device` unless listed here.
    pub states: HashMap<String, String>,
    /// `FAIL` texts `get-state` answers for a serial, in order, before it
    /// reports the real state.
    pub state_fails: HashMap<String, Vec<String>>,
    /// `host-serial:<s>:features`; `None` answers `shell_v2,cmd,stat_v2`.
    pub features: Option<String>,
    /// Answers `FAIL <msg>` right after `SEND` and then stops reading.
    pub sync_fail_early: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushedFile {
    pub serial: String,
    pub path: String,
    pub mode: u32,
    pub data: Vec<u8>,
    pub chunks: Vec<usize>,
    pub mtime: u32,
}

#[derive(Default)]
struct State {
    connected: HashMap<String, String>,
    state_fails: HashMap<String, std::collections::VecDeque<String>>,
    requests: Vec<String>,
    pushed: Vec<PushedFile>,
}

pub struct FakeAdbServer {
    pub port: u16,
    script: Arc<FakeAdbScript>,
    state: Arc<Mutex<State>>,
    accept: tokio::task::JoinHandle<()>,
}

impl FakeAdbServer {
    pub async fn start(script: FakeAdbScript) -> FakeAdbServer {
        Self::start_on(0, script).await
    }

    /// On 127.0.0.1:`port` (0: any free port).
    pub async fn start_on(port: u16, script: FakeAdbScript) -> FakeAdbServer {
        let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let script = Arc::new(script);
        let state = Arc::new(Mutex::new(State::default()));
        for (serial, fails) in &script.state_fails {
            state
                .lock()
                .unwrap()
                .state_fails
                .insert(serial.clone(), fails.iter().cloned().collect());
        }
        let (s, st) = (script.clone(), state.clone());
        let accept = tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    return;
                };
                let (s, st) = (s.clone(), st.clone());
                tokio::spawn(async move {
                    let _ = serve(sock, s, st).await;
                });
            }
        });
        FakeAdbServer {
            port,
            script,
            state,
            accept,
        }
    }

    /// Stops listening, like a server that died: new connections are refused.
    pub async fn stop(&self) {
        self.accept.abort();
        while TcpStream::connect(("127.0.0.1", self.port)).await.is_ok() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    pub fn pushed(&self) -> Vec<PushedFile> {
        self.state.lock().unwrap().pushed.clone()
    }

    pub fn requests(&self) -> Vec<String> {
        self.state.lock().unwrap().requests.clone()
    }
}

type Io = std::io::Result<()>;

async fn read_request(sock: &mut TcpStream) -> std::io::Result<String> {
    let mut len = [0u8; 4];
    sock.read_exact(&mut len).await?;
    let len = usize::from_str_radix(std::str::from_utf8(&len).unwrap(), 16).unwrap();
    let mut buf = vec![0u8; len];
    sock.read_exact(&mut buf).await?;
    Ok(String::from_utf8(buf).unwrap())
}

fn framed(msg: &str) -> Vec<u8> {
    let mut out = format!("{:04x}", msg.len()).into_bytes();
    out.extend_from_slice(msg.as_bytes());
    out
}

async fn okay_with(sock: &mut TcpStream, msg: &str) -> Io {
    let mut out = b"OKAY".to_vec();
    out.extend(framed(msg));
    sock.write_all(&out).await
}

async fn fail(sock: &mut TcpStream, msg: &str) -> Io {
    let mut out = b"FAIL".to_vec();
    out.extend(framed(msg));
    sock.write_all(&out).await
}

/// Closes the write half, then drains what the client still sends so the
/// close is a clean FIN, never a reset that could eat unread replies.
async fn finish(mut sock: TcpStream) -> Io {
    sock.shutdown().await?;
    let mut sink = [0u8; 1024];
    while sock.read(&mut sink).await? > 0 {}
    Ok(())
}

fn state_of(script: &FakeAdbScript, state: &Mutex<State>, serial: &str) -> Option<String> {
    if let Some(s) = script.states.get(serial) {
        return Some(s.clone());
    }
    state.lock().unwrap().connected.get(serial).cloned()
}

async fn serve(mut sock: TcpStream, script: Arc<FakeAdbScript>, state: Arc<Mutex<State>>) -> Io {
    let req = read_request(&mut sock).await?;
    state.lock().unwrap().requests.push(req.clone());
    if req == "host:version" {
        okay_with(&mut sock, &format!("{:04x}", script.version)).await?;
    } else if let Some(addr) = req.strip_prefix("host:connect:") {
        match script.connect.get(addr) {
            Some(Ok(serial)) => {
                state
                    .lock()
                    .unwrap()
                    .connected
                    .insert(serial.clone(), "device".into());
                okay_with(&mut sock, &format!("connected to {serial}")).await?;
            }
            Some(Err(msg)) => okay_with(&mut sock, msg).await?,
            None => {
                okay_with(
                    &mut sock,
                    &format!("failed to connect to '{addr}': No route to host"),
                )
                .await?
            }
        }
    } else if let Some(rest) = req.strip_prefix("host:pair:") {
        let (code, addr) = rest.split_once(':').unwrap();
        match script.pair.get(&(code.to_string(), addr.to_string())) {
            Some(Ok(())) => {
                okay_with(
                    &mut sock,
                    &format!("Successfully paired to {addr} [guid=adb-TEST]"),
                )
                .await?
            }
            Some(Err(msg)) => okay_with(&mut sock, msg).await?,
            None => {
                okay_with(
                    &mut sock,
                    "Failed: Wrong password or connection was dropped.",
                )
                .await?
            }
        }
    } else if let Some(addr) = req.strip_prefix("host:disconnect:") {
        state.lock().unwrap().connected.remove(addr);
        okay_with(&mut sock, &format!("disconnected {addr}")).await?;
    } else if let Some(serial) = req
        .strip_prefix("host-serial:")
        .and_then(|r| r.strip_suffix(":get-state"))
    {
        let queued = state
            .lock()
            .unwrap()
            .state_fails
            .get_mut(serial)
            .and_then(|q| q.pop_front());
        if let Some(msg) = queued {
            fail(&mut sock, &msg).await?;
            return finish(sock).await;
        }
        match state_of(&script, &state, serial) {
            Some(s) => okay_with(&mut sock, &s).await?,
            None => fail(&mut sock, &format!("device '{serial}' not found")).await?,
        }
    } else if let Some(serial) = req
        .strip_prefix("host-serial:")
        .and_then(|r| r.strip_suffix(":features"))
    {
        let _ = serial;
        let features = script.features.as_deref().unwrap_or("shell_v2,cmd,stat_v2");
        okay_with(&mut sock, features).await?;
    } else if let Some(serial) = req.strip_prefix("host:transport:") {
        if state_of(&script, &state, serial).as_deref() != Some("device") {
            fail(&mut sock, &format!("device '{serial}' not found")).await?;
            return finish(sock).await;
        }
        sock.write_all(b"OKAY").await?;
        let service = read_request(&mut sock).await?;
        state.lock().unwrap().requests.push(service.clone());
        if let Some(cmd) = service.strip_prefix("shell,v2,raw:") {
            sock.write_all(b"OKAY").await?;
            return serve_shell(sock, &script, cmd).await;
        } else if service == "sync:" {
            sock.write_all(b"OKAY").await?;
            return serve_sync(sock, &script, &state, serial).await;
        }
        fail(&mut sock, "unknown service").await?;
    } else {
        fail(&mut sock, "unknown host service").await?;
    }
    finish(sock).await
}

fn packet(id: u8, data: &[u8]) -> Vec<u8> {
    let mut out = vec![id];
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    out
}

async fn serve_shell(mut sock: TcpStream, script: &FakeAdbScript, cmd: &str) -> Io {
    let Some(reply) = script.shell.get(cmd) else {
        sock.write_all(&packet(
            2,
            format!("/system/bin/sh: {cmd}: not found\n").as_bytes(),
        ))
        .await?;
        sock.write_all(&packet(3, &[127])).await?;
        return finish(sock).await;
    };
    let bytes = match &reply.raw {
        Some(raw) => raw.clone(),
        None => {
            // stdout in packets of at most 16 KiB with stderr after the
            // first, as a real interleaved command would.
            let out = reply.stdout.as_bytes();
            let mut chunks = out.chunks(16 * 1024);
            let mut bytes = Vec::new();
            bytes.extend(packet(1, chunks.next().unwrap_or(&[])));
            if !reply.stderr.is_empty() {
                bytes.extend(packet(2, reply.stderr.as_bytes()));
            }
            for c in chunks {
                bytes.extend(packet(1, c));
            }
            if let Some(code) = reply.exit {
                bytes.extend(packet(3, &[code]));
            }
            bytes
        }
    };
    if reply.trickle {
        for b in &bytes {
            sock.write_all(std::slice::from_ref(b)).await?;
            sock.flush().await?;
        }
    } else {
        sock.write_all(&bytes).await?;
    }
    if reply.hang {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
    finish(sock).await
}

async fn read_header(sock: &mut TcpStream) -> std::io::Result<([u8; 4], u32)> {
    let mut hdr = [0u8; 8];
    sock.read_exact(&mut hdr).await?;
    let id = [hdr[0], hdr[1], hdr[2], hdr[3]];
    Ok((id, u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]])))
}

async fn serve_sync(
    mut sock: TcpStream,
    script: &FakeAdbScript,
    state: &Mutex<State>,
    serial: &str,
) -> Io {
    loop {
        let (id, len) = match read_header(&mut sock).await {
            Ok(h) => h,
            Err(_) => return Ok(()),
        };
        match &id {
            b"SEND" => {
                let mut buf = vec![0u8; len as usize];
                sock.read_exact(&mut buf).await?;
                let spec = String::from_utf8(buf).unwrap();
                let (path, mode) = spec.rsplit_once(',').unwrap();
                let mut file = PushedFile {
                    serial: serial.into(),
                    path: path.into(),
                    mode: mode.parse().unwrap(),
                    data: Vec::new(),
                    chunks: Vec::new(),
                    mtime: 0,
                };
                if let Some(msg) = &script.sync_fail_early {
                    let mut out = b"FAIL".to_vec();
                    out.extend_from_slice(&(msg.len() as u32).to_le_bytes());
                    out.extend_from_slice(msg.as_bytes());
                    sock.write_all(&out).await?;
                    // Stop reading: the client's writes stall.
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    return Ok(());
                }
                loop {
                    let (id, len) = read_header(&mut sock).await?;
                    match &id {
                        b"DATA" => {
                            let mut chunk = vec![0u8; len as usize];
                            sock.read_exact(&mut chunk).await?;
                            file.chunks.push(chunk.len());
                            file.data.extend(chunk);
                        }
                        b"DONE" => {
                            file.mtime = len;
                            break;
                        }
                        other => panic!("unexpected sync id {other:?}"),
                    }
                }
                if let Some(msg) = &script.sync_fail {
                    let mut out = b"FAIL".to_vec();
                    out.extend_from_slice(&(msg.len() as u32).to_le_bytes());
                    out.extend_from_slice(msg.as_bytes());
                    sock.write_all(&out).await?;
                } else {
                    state.lock().unwrap().pushed.push(file);
                    sock.write_all(b"OKAY\0\0\0\0").await?;
                }
            }
            b"QUIT" => return finish(sock).await,
            other => panic!("unexpected sync request {other:?}"),
        }
    }
}
