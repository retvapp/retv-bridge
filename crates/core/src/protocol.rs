//! The wire protocol's fixed values and security limits. Names and values match
//! the TypeScript bridge's `protocol.ts`; `PROTOCOL.md` is the contract.

/// The bridge listens on loopback only.
pub const BRIDGE_HOST: &str = "127.0.0.1";
pub const BRIDGE_PORT: u16 = 5577;
/// Protocol v2 adds `installApk` (announced in `hello`'s `features`).
pub const PROTOCOL_VERSION: u32 = 2;
/// What `hello` announces on top of protocol v1.
pub const FEATURES: &[&str] = &["installApk"];

/// Pages allowed to talk to the bridge (exact `Origin` header match).
pub const ALLOWED_ORIGINS: &[&str] = &[
    "http://127.0.0.1:5180",
    "http://localhost:5180",
    "http://127.0.0.1:4189",
    "https://retv.app",
    "https://ru.retv.app",
];

/// Host names the `Host` header may carry, always followed by `:<bound port>`
/// (DNS-rebinding guard). Case-sensitive.
pub const ALLOWED_HOSTS: &[&str] = &["127.0.0.1", "localhost"];

/// Largest accepted WebSocket message; the socket closes on anything bigger.
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
/// Largest shell command accepted in one `run`.
pub const MAX_COMMAND_CHARS: usize = 256 * 1024;
/// `writeFile` may only write below this directory on the TV.
pub const DEVICE_WRITE_DIR: &str = "/sdcard/ReTV/";
/// `writeBytes` may also write these exact staging files.
pub const DEVICE_STAGING_FILES: &[&str] = &["/data/local/tmp/retv-home.apk"];
/// Largest file one `writeBytes` upload may assemble.
pub const MAX_UPLOAD_BYTES: usize = 32 * 1024 * 1024;

/// Largest APK `installApk` downloads.
pub const MAX_INSTALL_BYTES: u64 = 64 * 1024 * 1024;
/// Where `installApk` stages the APK before `pm install`.
pub const INSTALL_STAGING_FILE: &str = "/data/local/tmp/retv-install.apk";
/// `installApk` only fetches below these URL prefixes.
pub const APK_URL_PREFIXES: &[&str] = &[
    "https://retv.app/downloads/",
    "https://ru.retv.app/downloads/",
];

/// A site pairing code lives this long, then a new one is needed.
pub const PAIR_CODE_TTL_MS: u64 = 120_000;
/// Wrong `pairFinish` guesses allowed per code; the 5th burns it and locks pairing.
pub const PAIR_MAX_MISSES: u32 = 5;
/// The first lock after `PAIR_MAX_MISSES` misses; each further lock doubles it.
pub const PAIR_LOCK_MS: u64 = 30_000;
/// The longest lock: the doubling stops here.
pub const PAIR_LOCK_MAX_MS: u64 = 3_600_000;
/// Codes one bridge run may issue; after that pairing needs a restart (`pair-disabled`).
pub const PAIR_MAX_STARTS: u32 = 20;

/// adb's default TCP port, used when neither the client nor mDNS gives one.
pub const DEFAULT_ADB_PORT: u16 = 5555;
