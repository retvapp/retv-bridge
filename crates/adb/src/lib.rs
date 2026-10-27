//! ReTV Bridge adb.
//!
//! - A client for the adb host (server) protocol: the smart-socket requests
//!   `adb` itself sends to its server on 127.0.0.1, one TCP connection per
//!   request. Shell commands use shell protocol v2, file writes the sync service.
//! - `tools` and `server_proc`: Google's platform-tools, fetched on first run
//!   with a pinned SHA-256, and run as a private adb server on port 5039.
//! - `HttpsApkFetcher`: the `installApk` downloader (HTTPS, no redirects).

mod apk_fetch;
mod bridge_adb;
mod errors;
mod host;
mod revive;
mod shell_v2;
mod sync;

pub mod server_proc;
pub mod tools;

pub use apk_fetch::HttpsApkFetcher;
pub use bridge_adb::{BoxFuture, Discoverer, EventSink, RealAdb, RealDevice};
pub use errors::map_adb_failure;
pub use host::{AdbHost, LogSink, Timeouts};
pub use revive::{Restarter, ServerRevive, REVIVE_GAP};
pub use sync::PushError;
