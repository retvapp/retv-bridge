//! Which browser opens the site. Safari does not let an https page talk to
//! ws://127.0.0.1, so on macOS, when Safari is the default, the bridge opens
//! the site in Chrome, Edge or Firefox if one is installed.

/// Browsers tried instead of Safari, in this order (macOS bundle ids).
pub const PREFERRED: [&str; 3] = [
    "com.google.Chrome",
    "com.microsoft.edgemac",
    "org.mozilla.firefox",
];

/// The browser to open the site in; `None` means the system default.
pub fn pick(default_is_safari: bool, installed: &[&str]) -> Option<&'static str> {
    if !default_is_safari {
        return None;
    }
    PREFERRED.into_iter().find(|id| installed.contains(id))
}

/// Whether Safari handles https, from the LaunchServices handler list
/// (`com.apple.launchservices.secure.plist` converted to JSON). No https
/// handler means the system default, which is Safari.
pub fn default_is_safari_from_json(json: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return true;
    };
    let handler = v
        .get("LSHandlers")
        .and_then(|h| h.as_array())
        .into_iter()
        .flatten()
        .find(|h| h.get("LSHandlerURLScheme").and_then(|s| s.as_str()) == Some("https"))
        .and_then(|h| h.get("LSHandlerRoleAll").and_then(|r| r.as_str()));
    match handler {
        None => true,
        Some(id) => id.eq_ignore_ascii_case("com.apple.safari"),
    }
}

/// The app bundle each preferred browser installs as.
#[cfg(target_os = "macos")]
fn app_name(id: &str) -> &'static str {
    match id {
        "com.google.Chrome" => "Google Chrome.app",
        "com.microsoft.edgemac" => "Microsoft Edge.app",
        _ => "Firefox.app",
    }
}

/// The preferred browsers installed in /Applications or ~/Applications.
#[cfg(target_os = "macos")]
fn installed() -> Vec<&'static str> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    PREFERRED
        .into_iter()
        .filter(|id| {
            let app = app_name(id);
            std::path::Path::new("/Applications").join(app).exists()
                || home
                    .as_ref()
                    .is_some_and(|h| h.join("Applications").join(app).exists())
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn default_is_safari() -> bool {
    let Some(home) = std::env::var_os("HOME") else {
        return true;
    };
    let plist = std::path::Path::new(&home)
        .join("Library/Preferences/com.apple.LaunchServices/com.apple.launchservices.secure.plist");
    match std::process::Command::new("/usr/bin/plutil")
        .args(["-convert", "json", "-o", "-"])
        .arg(&plist)
        .output()
    {
        Ok(out) if out.status.success() => {
            default_is_safari_from_json(&String::from_utf8_lossy(&out.stdout))
        }
        _ => true,
    }
}

/// Opens `url` in the browser `pick` chooses (macOS), else the default one.
///
/// The site link carries the session token in its fragment, and this puts it in a process's argv: of
/// `/usr/bin/open` on macOS, which lives for milliseconds (the browser then gets the URL from LaunchServices as an
/// Apple Event, not on its command line); of `xdg-open` and possibly the browser it starts on Linux; of the browser
/// started for the link on Windows, where other standard accounts cannot read it. That is accepted (PROTOCOL.md, «The
/// session token»): the token is worth something only on this computer, against 127.0.0.1, and changes on every
/// start; a program of the same account can reach the Bridge anyway. Calling LaunchServices directly
/// (`LSOpenFromURLSpec`) would close the macOS window at the cost of unsafe FFI code.
pub fn open_url(app: &tauri::AppHandle, url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        if let Some(id) = pick(default_is_safari(), &installed()) {
            let status = std::process::Command::new("/usr/bin/open")
                .args(["-b", id, url])
                .status();
            if status.is_ok_and(|s| s.success()) {
                return Ok(());
            }
        }
    }
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_pick_prefers_chrome_only_when_default_is_safari() {
        let all = [
            "org.mozilla.firefox",
            "com.microsoft.edgemac",
            "com.google.Chrome",
        ];
        assert_eq!(pick(true, &all), Some("com.google.Chrome"));
        assert_eq!(pick(false, &all), None);
        assert_eq!(
            pick(true, &["org.mozilla.firefox", "com.microsoft.edgemac"]),
            Some("com.microsoft.edgemac")
        );
        assert_eq!(
            pick(true, &["org.mozilla.firefox"]),
            Some("org.mozilla.firefox")
        );
        assert_eq!(pick(true, &["com.apple.Safari", "com.brave.Browser"]), None);
        assert_eq!(pick(true, &[]), None);
    }

    #[test]
    fn reads_the_https_handler() {
        let chrome = r#"{"LSHandlers":[
            {"LSHandlerURLScheme":"mailto","LSHandlerRoleAll":"com.apple.mail"},
            {"LSHandlerURLScheme":"https","LSHandlerRoleAll":"com.google.chrome"}]}"#;
        assert!(!default_is_safari_from_json(chrome));
        let safari = r#"{"LSHandlers":[{"LSHandlerURLScheme":"https","LSHandlerRoleAll":"com.apple.safari"}]}"#;
        assert!(default_is_safari_from_json(safari));
        assert!(default_is_safari_from_json(r#"{"LSHandlers":[]}"#));
        assert!(default_is_safari_from_json("{}"));
        assert!(default_is_safari_from_json("not json"));
    }
}
