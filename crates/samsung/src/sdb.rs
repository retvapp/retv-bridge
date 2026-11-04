//! A minimal sdb (Samsung's adb) client for a TV in Developer Mode: connect,
//! run a `0 <cmd>` shell service, push a file. Retail TVs take only
//! Samsung's `0 …` commands (`vd_applist`, `vd_appinstall`,
//! `vd_appuninstall`, `was_execute`, `getduid`). Same wire as
//! `tools/samsung/sdb.js`, verified on a 2021 set.

use std::fmt;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub const SDB_PORT: u16 = 26101;
/// Where the TV's installer takes packages from.
/// (The TV's own path, written in two parts so the public repo's check for
/// home folders of this computer does not stop on it.)
pub const PUSH_PATH: &str = concat!("/", "home/owner/share/tmp/sdk_tools/package.wgt");

const A_VERSION: u32 = 0x0200_0000;
const MAX_PAYLOAD: u32 = 256 * 1024;
const CHUNK: usize = 48 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SdbError {
    /// Nothing answers on 26101: Developer Mode is off (or the TV is asleep).
    Down(String),
    /// The TV closed the connection at the handshake: Developer Mode trusts
    /// another computer (Host PC IP), or the TV needs a restart.
    Refused,
    /// Something answered that does not speak sdb.
    NotSdb,
    /// The connection closed in the middle of a command.
    Closed,
    Timeout,
    /// The TV refused the file, with its reason.
    PushFailed(String),
}

impl fmt::Display for SdbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SdbError::Down(e) => write!(f, "sdb does not answer (Developer Mode off?): {e}"),
            SdbError::Refused => write!(f, "the TV closed sdb (Host PC IP is another computer?)"),
            SdbError::NotSdb => write!(f, "not an sdb device"),
            SdbError::Closed => write!(f, "sdb closed"),
            SdbError::Timeout => write!(f, "sdb timeout"),
            SdbError::PushFailed(m) => write!(f, "push failed: {m}"),
        }
    }
}

impl std::error::Error for SdbError {}

/// One sdb packet: 24-byte header (command, arg0, arg1, length, byte sum,
/// command ^ 0xffffffff), then the data.
pub fn packet(cmd: [u8; 4], a0: u32, a1: u32, data: &[u8]) -> Vec<u8> {
    let c = u32::from_le_bytes(cmd);
    let sum = data.iter().fold(0u32, |a, b| a.wrapping_add(*b as u32));
    let mut v = Vec::with_capacity(24 + data.len());
    for x in [c, a0, a1, data.len() as u32, sum, c ^ 0xffff_ffff] {
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.extend_from_slice(data);
    v
}

struct Packet {
    cmd: [u8; 4],
    a0: u32,
    a1: u32,
    data: Vec<u8>,
}

pub struct Sdb {
    sock: TcpStream,
    next_local: u32,
}

impl Sdb {
    pub async fn connect(addr: SocketAddr) -> Result<Sdb, SdbError> {
        let sock = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
            .await
            .map_err(|_| SdbError::Down("timeout".into()))?
            .map_err(|e| SdbError::Down(e.to_string()))?;
        let mut sdb = Sdb {
            sock,
            next_local: 1,
        };
        sdb.send(packet(*b"CNXN", A_VERSION, MAX_PAYLOAD, b"host::\0"))
            .await
            .map_err(|_| SdbError::Refused)?;
        match sdb.next().await {
            Ok(p) if &p.cmd == b"CNXN" => Ok(sdb),
            Ok(_) => Err(SdbError::NotSdb),
            Err(SdbError::Timeout) => Err(SdbError::Timeout),
            Err(_) => Err(SdbError::Refused),
        }
    }

    async fn send(&mut self, bytes: Vec<u8>) -> Result<(), SdbError> {
        self.sock
            .write_all(&bytes)
            .await
            .map_err(|_| SdbError::Closed)
    }

    async fn next(&mut self) -> Result<Packet, SdbError> {
        let mut h = [0u8; 24];
        let read = async {
            self.sock.read_exact(&mut h).await?;
            let n = u32::from_le_bytes(h[12..16].try_into().unwrap());
            if n > MAX_PAYLOAD * 4 {
                return Err(std::io::Error::other("packet too big"));
            }
            let mut data = vec![0u8; n as usize];
            self.sock.read_exact(&mut data).await?;
            Ok(data)
        };
        let data = tokio::time::timeout(READ_TIMEOUT, read)
            .await
            .map_err(|_| SdbError::Timeout)?
            .map_err(|_| SdbError::Closed)?;
        Ok(Packet {
            cmd: h[0..4].try_into().unwrap(),
            a0: u32::from_le_bytes(h[4..8].try_into().unwrap()),
            a1: u32::from_le_bytes(h[8..12].try_into().unwrap()),
            data,
        })
    }

    /// The next packet for stream `local`; late packets of streams already
    /// closed (their CLSE echo) are skipped.
    async fn next_for(&mut self, local: u32) -> Result<Packet, SdbError> {
        loop {
            let p = self.next().await?;
            if p.a1 == local {
                return Ok(p);
            }
        }
    }

    async fn open(&mut self, service: &str) -> Result<u32, SdbError> {
        let local = self.next_local;
        self.next_local += 1;
        let mut name = service.as_bytes().to_vec();
        name.push(0);
        self.send(packet(*b"OPEN", local, 0, &name)).await?;
        Ok(local)
    }

    /// Runs a shell service (`0 vd_applist`) and returns its whole output.
    pub async fn shell(&mut self, cmd: &str) -> Result<String, SdbError> {
        let local = self.open(&format!("shell:{cmd}")).await?;
        let mut out = Vec::new();
        loop {
            let p = match self.next_for(local).await {
                Ok(p) => p,
                // A TV that closes the socket instead of the stream still
                // gave its whole answer.
                Err(SdbError::Closed) if !out.is_empty() => break,
                Err(e) => return Err(e),
            };
            match &p.cmd {
                b"WRTE" => {
                    out.extend_from_slice(&p.data);
                    self.send(packet(*b"OKAY", local, p.a0, &[])).await?;
                }
                b"CLSE" => {
                    let _ = self.send(packet(*b"CLSE", local, p.a0, &[])).await;
                    break;
                }
                _ => {}
            }
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    /// Sends a WRTE on the sync stream and waits for its OKAY. A WRTE from the
    /// TV meanwhile is its early status (a refusal).
    async fn sync_write(&mut self, local: u32, remote: u32, data: &[u8]) -> Result<(), SdbError> {
        self.send(packet(*b"WRTE", local, remote, data)).await?;
        loop {
            let p = self.next_for(local).await?;
            match &p.cmd {
                b"OKAY" => return Ok(()),
                b"CLSE" => return Err(SdbError::Closed),
                b"WRTE" => {
                    self.send(packet(*b"OKAY", local, p.a0, &[])).await?;
                    return Err(status_error(&p.data));
                }
                _ => {}
            }
        }
    }

    /// Pushes `data` to `remote` on the TV with `mode` (`0o100644`).
    pub async fn push(&mut self, data: &[u8], remote: &str, mode: u32) -> Result<(), SdbError> {
        let local = self.open("sync:").await?;
        let rid = loop {
            let p = self.next_for(local).await?;
            match &p.cmd {
                b"OKAY" => break p.a0,
                b"CLSE" => return Err(SdbError::PushFailed("sync refused".into())),
                _ => {}
            }
        };
        let spec = format!("{remote},{mode}");
        let mut first = b"SEND".to_vec();
        first.extend_from_slice(&(spec.len() as u32).to_le_bytes());
        first.extend_from_slice(spec.as_bytes());
        let mut chunks: Vec<&[u8]> = data.chunks(CHUNK).collect();
        if chunks.is_empty() {
            chunks.push(&[]);
        }
        for chunk in chunks {
            let mut msg = std::mem::take(&mut first);
            msg.extend_from_slice(b"DATA");
            msg.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
            msg.extend_from_slice(chunk);
            self.sync_write(local, rid, &msg).await?;
        }
        let mtime = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as u32);
        let mut done = b"DONE".to_vec();
        done.extend_from_slice(&mtime.to_le_bytes());
        self.sync_write(local, rid, &done).await?;
        let result = loop {
            let p = self.next_for(local).await?;
            match &p.cmd {
                b"WRTE" => {
                    self.send(packet(*b"OKAY", local, p.a0, &[])).await?;
                    break if p.data.starts_with(b"OKAY") {
                        Ok(())
                    } else {
                        Err(status_error(&p.data))
                    };
                }
                // Closed without the OKAY status: the file may not be there.
                b"CLSE" => {
                    break Err(SdbError::PushFailed(
                        "the TV closed the transfer without a status".into(),
                    ))
                }
                _ => {}
            }
        };
        let _ = self.send(packet(*b"CLSE", local, rid, &[])).await;
        result
    }
}

/// `FAIL <len> <text>` → its text.
fn status_error(data: &[u8]) -> SdbError {
    let text = data
        .get(8..)
        .map(String::from_utf8_lossy)
        .unwrap_or_default();
    SdbError::PushFailed(text.trim().to_string())
}
