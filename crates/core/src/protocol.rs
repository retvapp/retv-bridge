//! The wire protocol's fixed values and security limits. Names and values match
//! the TypeScript bridge's `protocol.ts`; `PROTOCOL.md` is the contract.

/// The bridge listens on loopback only.
pub const BRIDGE_HOST: &str = "127.0.0.1";
pub const BRIDGE_PORT: u16 = 5577;
/// Protocol v2 adds `installApk`; v3 the `run` allow-list (both announced in `hello`'s `features`).
pub const PROTOCOL_VERSION: u32 = 3;
/// What `hello` announces on top of protocol v1: `installApk` (v2) and `allowlist` (v3: `run` takes only
/// the command shapes in `commands.rs`, anything else is `command-not-allowed`).
/// `siteSignIn`: the allow-list takes ReTV Home's site sign-in broadcast and never logs its one-time code.
pub const FEATURES: &[&str] = &["installApk", "allowlist", "siteSignIn"];

/// Pages allowed to talk to the bridge (exact `Origin` header match): the ReTV sites.
pub const ALLOWED_ORIGINS: &[&str] = &["https://retv.app", "https://ru.retv.app"];

/// The local dev site and the site tests: allowed only by a debug build
/// (`DEV_BUILD`; audit 2026-11-01 L1), never by a release.
pub const DEV_ORIGINS: &[&str] = &[
    "http://127.0.0.1:5180",
    "http://localhost:5180",
    "http://127.0.0.1:4189",
];

/// Whether this build also takes `DEV_ORIGINS` (debug builds only). The app
/// and the CLI pass it as `ServerConfig::dev_origins`.
pub const DEV_BUILD: bool = cfg!(debug_assertions);

/// The Origins a bridge accepts: the ReTV sites, plus `DEV_ORIGINS` when `dev`.
pub fn allowed_origins(dev: bool) -> Vec<&'static str> {
    let mut all = ALLOWED_ORIGINS.to_vec();
    if dev {
        all.extend_from_slice(DEV_ORIGINS);
    }
    all
}

/// Whether `origin` (exact, case-sensitive) may open a socket.
pub fn origin_allowed(origin: &str, dev: bool) -> bool {
    ALLOWED_ORIGINS.contains(&origin) || (dev && DEV_ORIGINS.contains(&origin))
}

/// Sockets open without having sent the session token (a new socket counts
/// until it does); one more upgrade is refused with 503 (audit 2026-11-01 L3).
pub const MAX_UNPAIRED_SOCKETS: usize = 4;
/// A socket that has not sent the session token this long after it opened is
/// closed (1008). Pairing works across sockets, so the site just opens another.
pub const UNPAIRED_SOCKET_MS: u64 = 60_000;

/// Host names the `Host` header may carry, always followed by `:<bound port>`
/// (DNS-rebinding guard). Case-sensitive.
pub const ALLOWED_HOSTS: &[&str] = &["127.0.0.1", "localhost"];

/// Largest accepted WebSocket message; the socket closes on anything bigger.
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
/// Largest shell command accepted in one `run` (longer is `bad-request`; the allow-list then decides).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_origins_only_when_asked() {
        for o in DEV_ORIGINS {
            assert!(!origin_allowed(o, false), "{o}");
            assert!(origin_allowed(o, true), "{o}");
        }
        for o in ALLOWED_ORIGINS {
            assert!(origin_allowed(o, false), "{o}");
        }
        assert!(!origin_allowed("https://evil.example", true));
        assert_eq!(
            allowed_origins(false),
            ["https://retv.app", "https://ru.retv.app"]
        );
        assert_eq!(DEV_BUILD, cfg!(debug_assertions));
    }
}
