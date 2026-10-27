//! Manual check: `cargo run -p retv-bridge-adb --example fetch -- <dir> [--stop] [--zip <file>]`.
//! Downloads the pinned platform-tools for this OS from dl.google.com into
//! `<dir>` (or installs `<file>` if it matches the pin), then starts adb on
//! 5039. The server keeps running unless `--stop` is given.

use std::path::PathBuf;
use std::sync::Arc;

use retv_bridge_adb::server_proc::{start_adb_server, BRIDGE_ADB_PORT};
use retv_bridge_adb::tools::{ensure_adb, host_pin, ReqwestGet, ToolsSource};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = PathBuf::from(
        args.first()
            .ok_or("usage: fetch <dir> [--stop] [--zip <file>]")?,
    );
    let stop = args.iter().any(|a| a == "--stop");
    let src = match args.iter().position(|a| a == "--zip") {
        Some(i) => {
            ToolsSource::LocalZip(PathBuf::from(args.get(i + 1).ok_or("--zip needs a file")?))
        }
        None => ToolsSource::Download(Arc::new(ReqwestGet::new()?)),
    };

    let pin = host_pin();
    println!("platform-tools r{} from {}", pin.revision, pin.url);
    let adb = ensure_adb(&dir, pin, src, |got, total| {
        if got == total || got % (1 << 20) < 16 * 1024 {
            eprint!("\r{got} / {total} bytes");
        }
    })
    .await?;
    eprintln!();
    println!("adb: {}", adb.display());

    let server = start_adb_server(&adb, BRIDGE_ADB_PORT).await?;
    println!("adb server answering on 127.0.0.1:{}", server.port());
    if stop {
        server.stop().await?;
        println!("stopped");
    }
    Ok(())
}
