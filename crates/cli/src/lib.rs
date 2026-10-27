//! `retv-bridge-cli`: the bridge without a window (Linux, or running from
//! source). The pieces `main.rs` puts together, kept here to be tested: the
//! arguments, the session token, the site link and the pairing code box.
//! It prints like the TS reference bridge's `main.ts`.

use std::path::PathBuf;

use rand::rngs::OsRng;
use rand::RngCore;
use retv_bridge_core::protocol::{ALLOWED_ORIGINS, BRIDGE_PORT};

/// The site opened by default (the local dev server, as in the TS bridge).
pub const DEFAULT_SITE: &str = "http://127.0.0.1:5180";
/// A re-shown code (pairStart again while it is live) is printed again at most this often.
pub const REPRINT_MS: u64 = 10_000;

pub const USAGE: &str = "usage: retv-bridge-cli [--port 5577] [--no-open] [--site URL]

  --port N     listen on 127.0.0.1:N (default 5577)
  --no-open    do not open the browser; the site then pairs with a 6-digit code
  --site URL   the ReTV site to open (default: RETV_SITE_URL or http://127.0.0.1:5180)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub port: u16,
    pub open: bool,
    pub site: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgsError {
    Help,
    Bad(String),
}

pub fn parse_args<I: IntoIterator<Item = String>>(argv: I) -> Result<Args, ArgsError> {
    let mut args = Args {
        port: BRIDGE_PORT,
        open: true,
        site: None,
    };
    let mut it = argv.into_iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (a.clone(), None),
        };
        let mut value = |name: &str| {
            inline
                .clone()
                .or_else(|| it.next())
                .ok_or_else(|| ArgsError::Bad(format!("{name} needs a value")))
        };
        match flag.as_str() {
            "--port" => {
                let v = value("--port")?;
                args.port = v
                    .parse::<u16>()
                    .ok()
                    .filter(|p| *p > 0)
                    .ok_or_else(|| ArgsError::Bad(format!("--port: not a port: {v}")))?;
            }
            "--site" => args.site = Some(value("--site")?),
            "--no-open" if inline.is_none() => args.open = false,
            "-h" | "--help" => return Err(ArgsError::Help),
            _ => return Err(ArgsError::Bad(format!("unknown argument: {a}"))),
        }
    }
    Ok(args)
}

/// A fresh session token of 120 random bits, base64url without padding.
pub fn new_token() -> String {
    let mut bytes = [0u8; 15];
    OsRng.fill_bytes(&mut bytes);
    base64url(&bytes)
}

fn base64url(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        let chars = c.len() + 1;
        for i in 0..chars {
            out.push(A[(n >> (18 - 6 * i)) as usize & 63] as char);
        }
    }
    out
}

/// The site's origin when it is on the bridge's allow-list, else `None`: the
/// token only ever goes to a ReTV site.
pub fn site_to_open(site: &str) -> Option<String> {
    let (scheme, rest) = site.split_once("://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let mut authority = rest[..end].to_ascii_lowercase();
    if authority.contains('@') {
        return None;
    }
    let scheme = scheme.to_ascii_lowercase();
    // The default port is not part of an origin.
    let default = if scheme == "https" { ":443" } else { ":80" };
    if let Some(stripped) = authority.strip_suffix(default) {
        authority = stripped.to_string();
    }
    let origin = format!("{scheme}://{authority}");
    ALLOWED_ORIGINS.contains(&origin.as_str()).then_some(origin)
}

/// The link the bridge opens: the token in the fragment, which browsers never
/// send to the site's server.
pub fn connect_link(origin: &str, token: &str) -> String {
    format!("{origin}/connect?path=bridge#bridgeToken={token}") // leakcheck:allow
}

/// The site pairing code as a framed block: `Code for the site: 482 917`.
pub fn code_block(code: &str) -> String {
    let (a, b) = code.split_at(code.len().min(3));
    let line = format!("Code for the site: {a} {b}");
    let bar = "═".repeat(line.chars().count() + 4);
    format!("╔{bar}╗\n║  {line}  ║\n╚{bar}╝")
}

/// Whether a shown code is printed again: a new code always, the same one
/// after `REPRINT_MS`.
pub fn should_print(last: Option<(&str, u64)>, code: &str, now_ms: u64) -> bool {
    match last {
        None => true,
        Some((c, at)) => c != code || now_ms.saturating_sub(at) >= REPRINT_MS,
    }
}

/// Where the bridge keeps its platform-tools: the OS's per-user data folder.
pub fn data_dir() -> PathBuf {
    let env = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let base = if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Application Support"))
    } else if cfg!(windows) {
        env("LOCALAPPDATA")
    } else {
        env("XDG_DATA_HOME").or_else(|| env("HOME").map(|h| h.join(".local/share")))
    };
    base.unwrap_or_else(std::env::temp_dir).join("retv-bridge")
}

/// Opens `url` in the default browser; failures are only reported.
pub fn open_browser(url: &str) -> std::io::Result<()> {
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Result<Args, ArgsError> {
        parse_args(a.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_the_flags() {
        assert_eq!(
            args(&[]),
            Ok(Args {
                port: 5577,
                open: true,
                site: None
            })
        );
        assert_eq!(
            args(&["--port", "6000", "--no-open", "--site", "https://retv.app"]),
            Ok(Args {
                port: 6000,
                open: false,
                site: Some("https://retv.app".into())
            })
        );
        assert_eq!(args(&["--port=6001"]).unwrap().port, 6001);
        assert!(matches!(args(&["--port", "0"]), Err(ArgsError::Bad(_))));
        assert!(matches!(args(&["--port", "x"]), Err(ArgsError::Bad(_))));
        assert!(matches!(args(&["--port"]), Err(ArgsError::Bad(_))));
        assert!(matches!(args(&["--what"]), Err(ArgsError::Bad(_))));
        assert_eq!(args(&["--help"]), Err(ArgsError::Help));
    }

    #[test]
    fn tokens_are_120_bit_base64url() {
        let t = new_token();
        assert_eq!(t.len(), 20);
        assert!(t
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
        assert_ne!(t, new_token());
        assert_eq!(base64url(b"\xfb\xff"), "-_8");
    }

    #[test]
    fn only_allowed_sites_get_the_token() {
        assert_eq!(
            site_to_open("http://127.0.0.1:5180"),
            Some("http://127.0.0.1:5180".into())
        );
        assert_eq!(
            site_to_open("https://retv.app/connect"),
            Some("https://retv.app".into())
        );
        assert_eq!(
            site_to_open("https://RETV.app:443/"),
            Some("https://retv.app".into())
        );
        assert_eq!(site_to_open("https://retv.app.evil.example"), None);
        assert_eq!(site_to_open("https://user@retv.app"), None);
        assert_eq!(site_to_open("http://retv.app"), None);
        assert_eq!(site_to_open("not a url"), None);
        assert_eq!(
            connect_link("https://retv.app", "T"),
            "https://retv.app/connect?path=bridge#bridgeToken=T" // leakcheck:allow
        );
    }

    #[test]
    fn code_box_and_reprints() {
        let b = code_block("482917");
        assert!(b.contains("║  Code for the site: 482 917  ║"), "{b}");
        assert_eq!(b.lines().count(), 3);
        assert!(should_print(None, "1", 0));
        assert!(!should_print(Some(("1", 0)), "1", 9_999));
        assert!(should_print(Some(("1", 0)), "1", 10_000));
        assert!(should_print(Some(("1", 0)), "2", 1));
    }
}
