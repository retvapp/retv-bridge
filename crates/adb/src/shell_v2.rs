//! Shell protocol v2: packets of `id: u8`, `len: u32 LE`, then `len` bytes.
//! stdout is id 1, stderr id 2, the exit code id 3 (one byte).

use retv_bridge_core::api::{ShellResult, TransportError};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

use crate::errors::stream_error;
use crate::host::{read_exact_idle, AdbHost};

const ID_STDOUT: u8 = 1;
const ID_STDERR: u8 = 2;
const ID_EXIT: u8 = 3;
const ID_CLOSE_STDIN: u8 = 4;

/// adbd's packets are far smaller; anything larger is a broken stream.
const MAX_PACKET: usize = 1024 * 1024;
/// stdout and stderr together.
const MAX_OUTPUT: usize = 16 * 1024 * 1024;

/// Reads packets until the exit packet. A socket that ends or goes silent
/// for the idle timeout first means the command's fate is unknown:
/// `ConnectionLost`.
pub(crate) async fn run(
    host: &AdbHost,
    mut sock: TcpStream,
) -> Result<ShellResult, TransportError> {
    let idle = host.timeouts.idle;
    // Nothing is ever written to stdin; closing it keeps commands that read
    // stdin from waiting forever. A failed write shows up as a failed read.
    let _ = sock.write_all(&[ID_CLOSE_STDIN, 0, 0, 0, 0]).await;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    loop {
        let mut hdr = [0u8; 5];
        read_exact_idle(&mut sock, &mut hdr, idle)
            .await
            .map_err(stream_error)?;
        let len = u32::from_le_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]) as usize;
        if len > MAX_PACKET {
            return Err(host.internal(format!(
                "shell v2 packet of {len} bytes (id {}) is over the {MAX_PACKET}-byte cap",
                hdr[0]
            )));
        }
        if stdout.len() + stderr.len() + len > MAX_OUTPUT {
            return Err(host.internal(format!("shell output is over the {MAX_OUTPUT}-byte cap")));
        }
        let mut data = vec![0u8; len];
        read_exact_idle(&mut sock, &mut data, idle)
            .await
            .map_err(stream_error)?;
        match hdr[0] {
            ID_STDOUT => stdout.extend(data),
            ID_STDERR => stderr.extend(data),
            ID_EXIT => {
                let code = *data.first().ok_or(TransportError::ConnectionLost)?;
                return Ok(ShellResult {
                    exit_code: i32::from(code),
                    stdout: String::from_utf8_lossy(&stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&stderr).into_owned(),
                });
            }
            _ => {}
        }
    }
}
