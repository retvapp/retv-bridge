//! The `run` allow-list (protocol v3, feature `allowlist`; `PROTOCOL.md` "Protocol v3"). The site may only run
//! the command shapes the ReTV engine sends. A command is split into words with a small, strict subset of sh
//! syntax, every word is checked against one of the `SHAPES` below, and the bridge runs the line rebuilt from
//! those checked words (quoted where needed), never the text it received. Anything else is `command-not-allowed`.
//! A port of the TS bridge's `commands.ts` (same table, same order); pinned by `conformance/commands.json`.

/// A word check: a literal, one of several literals, or a typed value.
#[derive(Clone, Copy)]
enum Part {
    Lit(&'static str),
    OneOf(&'static [&'static str]),
    Pkg,
    Component,
    Prop,
    Seconds,
    ProfileUser,
    Hex64,
    Chunk,
    PkgList,
    AnyUser,
    BackupB64,
    Scale,
    Hostname,
    A11yList,
    ScreensaverList,
    RetvFile,
    ApkPath,
    ApkEntry,
    MarketUrl,
}

use Part::*;

const RETV_DIR: &str = "/sdcard/ReTV/";
/// ReTV Home's layout backup in Download under its own name, and the «(n)» names a reinstall's copy gets.
const LAYOUT_BACKUPS: &[&str] = &[
    "/sdcard/Download/ReTV/retv-home-layout.json",
    "/sdcard/Download/ReTV/retv-home-layout (1).json",
    "/sdcard/Download/ReTV/retv-home-layout (2).json",
    "/sdcard/Download/ReTV/retv-home-layout (3).json",
    "/sdcard/Download/ReTV/retv-home-layout (4).json",
    "/sdcard/Download/ReTV/retv-home-layout (5).json",
    "/sdcard/Download/ReTV/retv-home-layout (6).json",
    "/sdcard/Download/ReTV/retv-home-layout (7).json",
    "/sdcard/Download/ReTV/retv-home-layout (8).json",
    "/sdcard/Download/ReTV/retv-home-layout (9).json",
];
const APK_ROOTS: &[&str] = &[
    "/data/app/",
    "/system/",
    "/system_ext/",
    "/product/",
    "/vendor/",
];
const MARKET_PREFIX: &str = "market://details?id=";

const LIST_FLAGS: &[&str] = &["-3", "-d", "-e", "-s", "-u"];
const DUMPSYS_SERVICES: &[&str] = &[
    "connectivity",
    "diskstats",
    "meminfo",
    "package",
    "usagestats",
    "webviewupdate",
    "wifi",
];
const QUERY_SERVICES: &[&str] = &[
    "android.net.VpnService",
    "android.accessibilityservice.AccessibilityService",
    "android.media.tv.TvInputService",
    "android.speech.RecognitionService",
    "android.service.voice.VoiceInteractionService",
    "android.service.dreams.DreamService",
];
const APPOPS: &[&str] = &["RECORD_AUDIO", "RUN_ANY_IN_BACKGROUND"];
const APPOPS_MODES: &[&str] = &["allow", "ignore", "deny", "default", "foreground"];
/// The settings ReTV reads and changes (the engine's `MANAGED_SETTINGS`, plus `adb_enabled` for «finish»).
const GLOBAL_KEYS: &[&str] = &[
    "window_animation_scale",
    "transition_animation_scale",
    "animator_duration_scale",
    "private_dns_mode",
    "private_dns_specifier",
    "always_finish_activities",
    "adb_enabled",
];
const SECURE_KEYS: &[&str] = &[
    "enabled_accessibility_services",
    "accessibility_enabled",
    "screensaver_components",
    "limit_ad_tracking",
];
const NAMESPACES: &[&str] = &["global", "secure", "system"];

/// The engine's microphone filter (`mic-audit.ts` `MIC_FILTER`): fixed `-e` patterns, word for word.
const MIC_FILTER: &[Part] = &[
    Lit("grep"),
    Lit("-e"),
    Lit(r"Package \["),
    Lit("-e"),
    Lit(r"SharedUser \["),
    Lit("-e"),
    Lit("userId="),
    Lit("-e"),
    Lit("sharedUser="),
    Lit("-e"),
    Lit("pkgFlags="),
    Lit("-e"),
    Lit("versionCode="),
    Lit("-e"),
    Lit("User "),
    Lit("-e"),
    Lit("RECORD_AUDIO"),
    Lit("-e"),
    Lit("^[A-Z]"),
];
const VERSION_FILTER: &[Part] = &[
    Lit("grep"),
    Lit("-E"),
    Lit(r"^ *(Package \[|versionCode=|versionName=)"),
];
const ICON_FILTER: &[Part] = &[
    Lit("grep"),
    Lit("-E"),
    Lit("res/(mipmap|drawable)[^/]*/(ic_launcher|app_icon|icon|ic_app|launcher_icon)(_round)?[.](png|webp)$"),
];
const RECORD_AUDIO: &str = "android.permission.RECORD_AUDIO";
const READ_LOGS: &str = "android.permission.READ_LOGS";
const READ_TV_LISTINGS: &str = "android.permission.READ_TV_LISTINGS";
/// The screens «Ad privacy» may open (ads privacy, privacy, then the settings root as the fallback).
const PRIVACY_ACTIONS: &[&str] = &[
    "com.google.android.gms.settings.ADS_PRIVACY",
    "android.settings.PRIVACY_SETTINGS",
    "android.settings.SETTINGS",
];

/// Every command the site may run, as word checks per pipeline stage. Kept in the same order as the TS table.
const SHAPES: &[&[&[Part]]] = &[
    // Reads: device facts.
    &[&[Lit("getprop")]],
    &[&[Lit("getprop"), Prop]],
    &[&[Lit("cat"), Lit("/proc/meminfo")]],
    &[&[Lit("dumpsys"), OneOf(DUMPSYS_SERVICES)]],
    &[&[Lit("dumpsys"), OneOf(&["meminfo", "package"]), Pkg]],
    &[&[Lit("dumpsys"), Lit("notification"), Lit("--noredact")]],
    &[&[Lit("dumpsys"), Lit("user")]],
    &[&[Lit("dumpsys"), Lit("package")], MIC_FILTER],
    &[&[Lit("dumpsys"), Lit("package"), Pkg], MIC_FILTER],
    &[&[Lit("dumpsys"), Lit("package"), Pkg], VERSION_FILTER],
    &[&[Lit("pm"), Lit("list"), Lit("users")]],
    &[&[Lit("pm"), Lit("list"), Lit("packages")]],
    &[&[Lit("pm"), Lit("list"), Lit("packages"), OneOf(LIST_FLAGS)]],
    &[&[
        Lit("pm"),
        Lit("list"),
        Lit("packages"),
        Lit("-d"),
        Lit("-u"),
    ]],
    &[&[Lit("pm"), Lit("list"), Lit("packages"), Pkg]],
    &[&[Lit("pm"), Lit("list"), Lit("packages"), Lit("-e"), Pkg]],
    // User 0 named (engine `detect.ts` USER0): without `--user`, Android 13+ lists every user's packages at once.
    &[&[
        Lit("pm"),
        Lit("list"),
        Lit("packages"),
        Lit("--user"),
        Lit("0"),
    ]],
    &[&[
        Lit("pm"),
        Lit("list"),
        Lit("packages"),
        OneOf(LIST_FLAGS),
        Lit("--user"),
        Lit("0"),
    ]],
    &[&[
        Lit("pm"),
        Lit("list"),
        Lit("packages"),
        Lit("--user"),
        Lit("0"),
        Pkg,
    ]],
    &[&[
        Lit("pm"),
        Lit("list"),
        Lit("packages"),
        Lit("-e"),
        Lit("--user"),
        Lit("0"),
        Pkg,
    ]],
    // Another Android user's packages and HOME (engine `snapshot.ts`, a v2 snapshot); user 0 has its shapes above.
    &[&[
        Lit("pm"),
        Lit("list"),
        Lit("packages"),
        OneOf(&["-d", "-e", "-u"]),
        Lit("--user"),
        ProfileUser,
    ]],
    &[&[
        Lit("pm"),
        Lit("list"),
        Lit("packages"),
        Lit("--user"),
        ProfileUser,
    ]],
    &[&[
        Lit("cmd"),
        Lit("package"),
        Lit("resolve-activity"),
        Lit("--brief"),
        Lit("--user"),
        ProfileUser,
        Lit("-a"),
        Lit("android.intent.action.MAIN"),
        Lit("-c"),
        Lit("android.intent.category.HOME"),
    ]],
    // Another Android user's HOME answerers (engine `restore.ts`: a restricted or full user's HOME set back).
    &[&[
        Lit("cmd"),
        Lit("package"),
        Lit("query-activities"),
        Lit("--brief"),
        Lit("--user"),
        ProfileUser,
        Lit("-a"),
        Lit("android.intent.action.MAIN"),
        Lit("-c"),
        Lit("android.intent.category.HOME"),
    ]],
    &[&[Lit("pm"), Lit("path"), Pkg]],
    &[&[Lit("pm"), Lit("path"), Lit("--user"), Lit("current"), Pkg]],
    &[&[
        Lit("pm"),
        Lit("query-services"),
        Lit("--components"),
        Lit("-a"),
        OneOf(QUERY_SERVICES),
    ]],
    &[&[
        Lit("cmd"),
        Lit("package"),
        Lit("query-activities"),
        Lit("--brief"),
        Lit("-a"),
        Lit("android.intent.action.VOICE_COMMAND"),
    ]],
    &[&[
        Lit("cmd"),
        Lit("package"),
        OneOf(&["query-activities", "resolve-activity"]),
        Lit("--brief"),
        Lit("-a"),
        Lit("android.intent.action.MAIN"),
        Lit("-c"),
        Lit("android.intent.category.HOME"),
    ]],
    &[&[
        Lit("content"),
        Lit("query"),
        Lit("--uri"),
        Lit("content://android.media.tv/channel"),
        Lit("--projection"),
        Lit("package_name:display_name:browsable:type"),
    ]],
    &[&[Lit("ime"), Lit("list"), Lit("-a"), Lit("-s")]],
    &[&[Lit("cmd"), Lit("appops"), Lit("get"), Pkg, OneOf(APPOPS)]],
    &[&[
        Lit("cmd"),
        Lit("appops"),
        Lit("get"),
        Lit("--user"),
        Lit("0"),
        Pkg,
        OneOf(APPOPS),
    ]],
    &[&[
        Lit("cmd"),
        Lit("appops"),
        Lit("query-op"),
        OneOf(APPOPS),
        OneOf(APPOPS_MODES),
    ]],
    &[&[
        Lit("cmd"),
        Lit("appops"),
        Lit("query-op"),
        Lit("--user"),
        Lit("0"),
        OneOf(APPOPS),
        OneOf(APPOPS_MODES),
    ]],
    &[&[
        Lit("settings"),
        Lit("get"),
        Lit("global"),
        OneOf(GLOBAL_KEYS),
    ]],
    &[&[
        Lit("settings"),
        Lit("get"),
        Lit("secure"),
        OneOf(SECURE_KEYS),
    ]],
    &[&[Lit("settings"), Lit("list"), OneOf(NAMESPACES)]],
    // App icons on the Installed screen.
    &[
        &[Lit("unzip"), Lit("-l"), ApkPath, Lit("res/*")],
        ICON_FILTER,
        &[Lit("head"), Lit("-n"), Lit("64")],
    ],
    &[
        &[Lit("unzip"), Lit("-p"), ApkPath, ApkEntry],
        &[Lit("base64")],
    ],
    // Snapshots in /sdcard/ReTV/ (writeFile writes them).
    &[&[Lit("ls"), Lit(RETV_DIR)]],
    &[&[Lit("cat"), RetvFile]],
    // ReTV Home's own layout backups in Download (engine google-tv.ts sendLayoutBackup), read with a bound to hand the
    // newest back after a reinstall: the folder's names, then at most 72001 bytes of one of ReTV Home's own file names.
    &[&[Lit("ls"), Lit("/sdcard/Download/ReTV/")]],
    &[&[Lit("head"), Lit("-c"), Lit("72001"), OneOf(LAYOUT_BACKUPS)]],
    &[&[Lit("mv"), RetvFile, RetvFile]],
    &[&[Lit("rm"), Lit("-f"), RetvFile]],
    // The v1 ReTV Home upload's staging file (writeBytes may write it; installing from it is not allowed).
    &[&[Lit("rm"), Lit("-f"), Lit("/data/local/tmp/retv-home.apk")]],
    // Changes: packages and components.
    &[&[
        Lit("pm"),
        OneOf(&["disable-user", "enable"]),
        Lit("--user"),
        Lit("0"),
        Pkg,
    ]],
    // Another Android user's apps (engine `apply.ts`, a profile plan: disables only, enabled again on a revert).
    &[&[
        Lit("pm"),
        OneOf(&["disable-user", "enable"]),
        Lit("--user"),
        ProfileUser,
        Pkg,
    ]],
    &[&[
        Lit("pm"),
        OneOf(&["disable", "disable-user", "default-state", "enable"]),
        Lit("--user"),
        Lit("0"),
        Component,
    ]],
    &[&[
        Lit("cmd"),
        Lit("package"),
        Lit("install-existing"),
        Lit("--user"),
        Lit("0"),
        Pkg,
    ]],
    // Another Android user's app put back (engine `restore.ts`: a package removed from that user since the snapshot).
    &[&[
        Lit("cmd"),
        Lit("package"),
        Lit("install-existing"),
        Lit("--user"),
        ProfileUser,
        Pkg,
    ]],
    &[&[
        Lit("pm"),
        Lit("uninstall"),
        Lit("-k"),
        Lit("--user"),
        Lit("0"),
        Pkg,
    ]],
    // ReTV Home removed for another Android user (engine `launcher.ts` removeRetvHomeForUser): that package only.
    &[&[
        Lit("pm"),
        Lit("uninstall"),
        Lit("-k"),
        Lit("--user"),
        ProfileUser,
        Lit("app.retv.home"),
    ]],
    &[&[
        Lit("pm"),
        OneOf(&["grant", "revoke"]),
        Lit("--user"),
        Lit("0"),
        Pkg,
        Lit(RECORD_AUDIO),
    ]],
    &[&[
        Lit("pm"),
        OneOf(&["grant", "revoke"]),
        Lit("--user"),
        Lit("0"),
        Lit("app.retv.home"),
        Lit(READ_LOGS),
    ]],
    &[&[
        Lit("pm"),
        OneOf(&["grant", "revoke"]),
        Lit("--user"),
        Lit("0"),
        Lit("app.retv.home"),
        Lit(READ_TV_LISTINGS),
    ]],
    &[&[
        Lit("cmd"),
        Lit("appops"),
        Lit("set"),
        Pkg,
        OneOf(APPOPS),
        OneOf(APPOPS_MODES),
    ]],
    &[&[
        Lit("cmd"),
        Lit("appops"),
        Lit("set"),
        Lit("--user"),
        Lit("0"),
        Pkg,
        OneOf(APPOPS),
        OneOf(APPOPS_MODES),
    ]],
    // ReTV Home installs its own updates (engine google-tv.ts allowSelfUpdate / restoreSelfUpdate): that package and op only.
    &[&[
        Lit("cmd"),
        Lit("appops"),
        Lit("set"),
        Lit("app.retv.home"),
        Lit("REQUEST_INSTALL_PACKAGES"),
        OneOf(&["allow", "default", "ignore", "deny"]),
    ]],
    // ReTV Home's install permission, recorded by the snapshot (engine google-tv.ts readSelfUpdateMode): that package and op only.
    &[&[
        Lit("cmd"),
        Lit("appops"),
        Lit("get"),
        Lit("app.retv.home"),
        Lit("REQUEST_INSTALL_PACKAGES"),
    ]],
    &[&[
        Lit("cmd"),
        Lit("package"),
        Lit("set-home-activity"),
        Component,
    ]],
    &[&[
        Lit("cmd"),
        Lit("package"),
        Lit("set-home-activity"),
        Lit("--user"),
        Lit("0"),
        Component,
    ]],
    // Another Android user's HOME (engine `restore.ts`: a restricted or full user's HOME set back).
    &[&[
        Lit("cmd"),
        Lit("package"),
        Lit("set-home-activity"),
        Lit("--user"),
        ProfileUser,
        Component,
    ]],
    // Changes: settings.
    &[&[
        Lit("settings"),
        Lit("put"),
        Lit("global"),
        OneOf(&[
            "window_animation_scale",
            "transition_animation_scale",
            "animator_duration_scale",
        ]),
        Scale,
    ]],
    &[&[
        Lit("settings"),
        Lit("put"),
        Lit("global"),
        Lit("private_dns_mode"),
        OneOf(&["off", "opportunistic", "hostname"]),
    ]],
    &[&[
        Lit("settings"),
        Lit("put"),
        Lit("global"),
        Lit("private_dns_specifier"),
        Hostname,
    ]],
    &[&[
        Lit("settings"),
        Lit("put"),
        Lit("global"),
        OneOf(&["always_finish_activities", "adb_enabled"]),
        OneOf(&["0", "1"]),
    ]],
    &[&[
        Lit("settings"),
        Lit("put"),
        Lit("secure"),
        Lit("accessibility_enabled"),
        OneOf(&["0", "1"]),
    ]],
    &[&[
        Lit("settings"),
        Lit("put"),
        Lit("secure"),
        Lit("enabled_accessibility_services"),
        A11yList,
    ]],
    &[&[
        Lit("settings"),
        Lit("put"),
        Lit("secure"),
        Lit("screensaver_components"),
        ScreensaverList,
    ]],
    &[&[
        Lit("settings"),
        Lit("put"),
        Lit("secure"),
        Lit("limit_ad_tracking"),
        OneOf(&["0", "1"]),
    ]],
    &[&[
        Lit("settings"),
        Lit("delete"),
        Lit("global"),
        OneOf(GLOBAL_KEYS),
    ]],
    &[&[
        Lit("settings"),
        Lit("delete"),
        Lit("secure"),
        OneOf(SECURE_KEYS),
    ]],
    // Speed: caches and compilation, each time-limited.
    &[&[Lit("pm"), Lit("trim-caches"), Lit("999G")]],
    &[&[
        Lit("timeout"),
        Seconds,
        Lit("cmd"),
        Lit("package"),
        Lit("bg-dexopt-job"),
    ]],
    &[&[
        Lit("timeout"),
        Seconds,
        Lit("cmd"),
        Lit("package"),
        Lit("compile"),
        Lit("-m"),
        Lit("speed-profile"),
        Lit("-a"),
    ]],
    // ReTV Home: its four receivers (all need the shell's DUMP permission), and the Play Store page of an app.
    &[&[
        Lit("am"),
        Lit("broadcast"),
        Lit("-n"),
        Lit("app.retv.home/.data.DeviceKeyReceiver"),
        Lit("-a"),
        Lit("app.retv.home.action.DEVICE_KEY"),
        Lit("-f"),
        Lit("32"),
        Lit("--es"),
        Lit("key"),
        Hex64,
    ]],
    // The packages ReTV turned off, for ReTV Home's «ТВ в порядке» (engine `home-record.ts`), in numbered chunks.
    // Under `timeout 10`: a broadcast whose finish never comes (a stopped user) cannot hold the wizard.
    &[&[
        Lit("timeout"),
        Lit("10"),
        Lit("am"),
        Lit("broadcast"),
        Lit("--user"),
        AnyUser,
        Lit("-n"),
        Lit("app.retv.home/.data.DisabledSetReceiver"),
        Lit("-a"),
        Lit("app.retv.home.action.DISABLED_SET"),
        Lit("-f"),
        Lit("32"),
        Lit("--ei"),
        Lit("seq"),
        Chunk,
        Lit("--ei"),
        Lit("total"),
        Chunk,
        Lit("--es"),
        Lit("pkgs"),
        PkgList,
    ]],
    &[&[
        Lit("am"),
        Lit("broadcast"),
        Lit("-n"),
        Lit("app.retv.home/.data.LayoutBackupReceiver"),
        Lit("-a"),
        Lit("app.retv.home.action.LAYOUT_BACKUP"),
        Lit("-f"),
        Lit("32"),
        Lit("--es"),
        Lit("backup"),
        BackupB64,
    ]],
    &[&[
        Lit("am"),
        Lit("broadcast"),
        Lit("-n"),
        Lit("app.retv.home/.a11y.HomeOverrideReceiver"),
        Lit("-a"),
        Lit("app.retv.home.action.HOME_OVERRIDE"),
        Lit("-f"),
        Lit("32"),
        Lit("--ez"),
        Lit("consent"),
        OneOf(&["true", "false"]),
    ]],
    &[&[
        Lit("am"),
        Lit("start"),
        Lit("-a"),
        Lit("android.intent.action.VIEW"),
        Lit("-d"),
        MarketUrl,
    ]],
    &[&[Lit("am"), Lit("start"), Lit("-a"), OneOf(PRIVACY_ACTIONS)]],
];

fn all_bytes(s: &str, ok: impl Fn(u8) -> bool) -> bool {
    s.bytes().all(ok)
}

fn word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// A package name as the engine checks it: `^[A-Za-z_][A-Za-z0-9_]*(\.[A-Za-z0-9_]+)+$`, at most 255.
fn is_pkg(w: &str) -> bool {
    let mut segs = w.split('.');
    let first = segs.next().unwrap_or("");
    let rest: Vec<&str> = segs.collect();
    w.len() <= 255
        && !rest.is_empty()
        && first
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && all_bytes(first, word_byte)
        && rest
            .iter()
            .all(|s| !s.is_empty() && all_bytes(s, word_byte))
}

/// `<package>/<class>`; the class may be relative (`.Main`) and may hold `$` (an inner class).
fn is_component(w: &str) -> bool {
    match w.find('/') {
        Some(at) if at > 0 && w.len() <= 512 => {
            let cls = &w[at + 1..];
            is_pkg(&w[..at])
                && !cls.is_empty()
                && all_bytes(cls, |c| word_byte(c) || c == b'.' || c == b'$')
        }
        _ => false,
    }
}

/// A system property name for `getprop`: `^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$`.
fn is_prop(w: &str) -> bool {
    w.len() <= 128
        && w.bytes().next().is_some_and(word_byte)
        && all_bytes(w, |c| word_byte(c) || c == b'.' || c == b'-')
}

/// A whole number of seconds 1..3600 for `timeout`, no leading zero.
fn is_seconds(w: &str) -> bool {
    !w.is_empty()
        && w.len() <= 4
        && !w.starts_with('0')
        && all_bytes(w, |c| c.is_ascii_digit())
        && w.parse::<u32>().is_ok_and(|n| n <= 3600)
}

/// An Android user other than 0 (the engine's `userArg`): a whole number 1..999999, no leading zero.
fn is_profile_user(w: &str) -> bool {
    !w.is_empty() && w.len() <= 6 && !w.starts_with('0') && all_bytes(w, |c| c.is_ascii_digit())
}

fn is_hex64(w: &str) -> bool {
    w.len() == 64 && all_bytes(w, |c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// A chunk number or count for ReTV Home's disabled set: 1..99, no leading zero.
fn is_chunk(w: &str) -> bool {
    !w.is_empty() && w.len() <= 2 && !w.starts_with('0') && all_bytes(w, |c| c.is_ascii_digit())
}

/// One chunk of ReTV Home's disabled set: packages joined by commas (at most 1000 characters), or `-` for none.
fn is_pkg_list(w: &str) -> bool {
    w == "-" || (w.len() <= 1000 && w.split(',').all(is_pkg))
}

/// A layout backup handed to a reinstalled ReTV Home: standard base64, 4..96000 characters, padded
/// (`^[A-Za-z0-9+/]+={0,2}$`, a multiple of 4).
fn is_backup_b64(w: &str) -> bool {
    let body = w.trim_end_matches('=');
    (4..=96000).contains(&w.len())
        && w.len().is_multiple_of(4)
        && w.len() - body.len() <= 2
        && !body.is_empty()
        && all_bytes(body, |c| {
            c.is_ascii_alphanumeric() || c == b'+' || c == b'/'
        })
}

// Setting values, typed per key.

/// An animation scale: a decimal 0..10 (`0`, `0.5`, `1.0`, `10`), at most 8 characters:
/// `^(10(\.0+)?|[0-9](\.[0-9]+)?)$`.
fn is_scale(w: &str) -> bool {
    if w.len() > 8 {
        return false;
    }
    let (int, frac) = match w.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (w, None),
    };
    let frac_ok = |f: Option<&str>, digits: fn(u8) -> bool| {
        f.is_none_or(|f| !f.is_empty() && f.bytes().all(digits))
    };
    match int {
        "10" => frac_ok(frac, |c| c == b'0'),
        _ => {
            int.len() == 1
                && int.as_bytes()[0].is_ascii_digit()
                && frac_ok(frac, |c| c.is_ascii_digit())
        }
    }
}

/// A Private DNS host name: `^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9-]+)+$`, at most 253.
fn is_hostname(w: &str) -> bool {
    let label = |s: &str| {
        !s.is_empty()
            && all_bytes(s, |c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'
            })
    };
    let mut parts = w.split('.');
    let first = parts.next().unwrap_or("");
    let rest: Vec<&str> = parts.collect();
    w.len() <= 253
        && !rest.is_empty()
        && label(first)
        && !first.starts_with('-')
        && !first.ends_with('-')
        && rest.iter().all(|s| label(s))
}

/// Components joined by `sep`, or nothing (restore may write back an empty list).
fn is_component_list(w: &str, sep: char) -> bool {
    w.is_empty() || (w.len() <= 4096 && w.split(sep).all(is_component))
}

/// A file directly in /sdcard/ReTV/ (the snapshots and their `.tmp` copies).
fn is_retv_file(w: &str) -> bool {
    w.strip_prefix(RETV_DIR).is_some_and(|name| {
        !name.is_empty()
            && name.len() <= 128
            && name != "."
            && name != ".."
            && all_bytes(name, |c| word_byte(c) || c == b'.' || c == b'-')
    })
}

/// An installed APK (`pm path` output) whose icon the site reads.
fn is_apk_path(w: &str) -> bool {
    w.len() <= 512
        && APK_ROOTS.iter().any(|root| w.starts_with(root))
        && w.ends_with(".apk")
        && w[1..].split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && all_bytes(seg, |c| word_byte(c) || b".~=+-".contains(&c))
        })
}

/// An icon inside an APK: `res/<dir>/<file>.png|webp`, the folder not `.` or `..`.
fn is_apk_entry(w: &str) -> bool {
    let name_ok =
        |s: &str| !s.is_empty() && all_bytes(s, |c| word_byte(c) || c == b'.' || c == b'-');
    let Some(rest) = w.strip_prefix("res/") else {
        return false;
    };
    let mut parts = rest.split('/');
    let (Some(dir), Some(file), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let stem = file
        .strip_suffix(".png")
        .or_else(|| file.strip_suffix(".webp"));
    name_ok(dir) && dir != "." && dir != ".." && stem.is_some_and(name_ok)
}

/// The Play Store page of one app.
fn is_market_url(w: &str) -> bool {
    w.strip_prefix(MARKET_PREFIX).is_some_and(is_pkg)
}

fn part_matches(part: Part, w: &str) -> bool {
    match part {
        Lit(s) => w == s,
        OneOf(list) => list.contains(&w),
        Pkg => is_pkg(w),
        Component => is_component(w),
        Prop => is_prop(w),
        Seconds => is_seconds(w),
        ProfileUser => is_profile_user(w),
        Hex64 => is_hex64(w),
        Chunk => is_chunk(w),
        PkgList => is_pkg_list(w),
        AnyUser => w == "0" || is_profile_user(w),
        BackupB64 => is_backup_b64(w),
        Scale => is_scale(w),
        Hostname => is_hostname(w),
        A11yList => is_component_list(w, ':'),
        ScreensaverList => is_component_list(w, ','),
        RetvFile => is_retv_file(w),
        ApkPath => is_apk_path(w),
        ApkEntry => is_apk_entry(w),
        MarketUrl => is_market_url(w),
    }
}

fn shape_matches(shape: &[&[Part]], stages: &[Vec<String>]) -> bool {
    shape.len() == stages.len()
        && shape.iter().zip(stages).all(|(parts, words)| {
            parts.len() == words.len() && parts.iter().zip(words).all(|(p, w)| part_matches(*p, w))
        })
}

/// Characters a word may hold outside quotes.
fn unquoted(c: u8) -> bool {
    word_byte(c) || b".,:/=+@?-".contains(&c)
}

/// Splits `cmd` into pipeline stages of words, or `None`. The syntax is a strict subset of sh: printable
/// ASCII only; words separated by exactly one space; a word joins unquoted runs of `[A-Za-z0-9_.,:/=+@?-]`,
/// single-quoted strings (anything printable but `'`) and `\'`; a lone unquoted `|` word separates stages.
pub fn split_command(cmd: &str) -> Option<Vec<Vec<String>>> {
    let b = cmd.as_bytes();
    if !b.iter().all(|c| (0x20..=0x7e).contains(c)) {
        return None;
    }
    let mut stages: Vec<Vec<String>> = vec![Vec::new()];
    let mut word = String::new();
    let mut started = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b' ' {
            if !started {
                return None;
            }
            stages.last_mut()?.push(std::mem::take(&mut word));
            started = false;
        } else if c == b'\'' {
            let close = i + 1 + b[i + 1..].iter().position(|&x| x == b'\'')?;
            word.push_str(&cmd[i + 1..close]);
            started = true;
            i = close;
        } else if c == b'\\' {
            if b.get(i + 1) != Some(&b'\'') {
                return None;
            }
            word.push('\'');
            started = true;
            i += 1;
        } else if c == b'|' && !started {
            if stages.last()?.is_empty() || b.get(i + 1) != Some(&b' ') {
                return None;
            }
            stages.push(Vec::new());
            i += 1; // the space after `|`
        } else if unquoted(c) {
            word.push(c as char);
            started = true;
        } else {
            return None;
        }
        i += 1;
    }
    if !started {
        return None;
    }
    stages.last_mut()?.push(word);
    Some(stages)
}

/// The word as the device shell must see it: bare when that is safe, else single-quoted.
fn quote(w: &str) -> String {
    let bare = !w.is_empty() && all_bytes(w, |c| word_byte(c) || b".,:/=+@-".contains(&c));
    if bare {
        w.to_string()
    } else {
        format!("'{}'", w.replace('\'', r"'\''"))
    }
}

/// The shell line the bridge runs for `cmd`, rebuilt from its checked words, or `None` when `cmd` is not allowed.
pub fn allow_command(cmd: &str) -> Option<String> {
    let stages = split_command(cmd)?;
    if !SHAPES.iter().any(|shape| shape_matches(shape, &stages)) {
        return None;
    }
    Some(
        stages
            .iter()
            .map(|words| words.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .join(" | "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebuilds_quotes_only_where_needed() {
        assert_eq!(
            allow_command("settings put global animator_duration_scale '1.0'").as_deref(),
            Some("settings put global animator_duration_scale 1.0")
        );
        assert_eq!(
            allow_command("pm enable --user 0 'a.b/.C$D'").as_deref(),
            Some("pm enable --user 0 'a.b/.C$D'")
        );
    }

    #[test]
    fn refuses_shell_syntax() {
        for cmd in [
            "getprop; id",
            "getprop | sh",
            "getprop $(id)",
            "getprop\tx",
            "getprop ",
        ] {
            assert_eq!(allow_command(cmd), None, "{cmd}");
        }
    }
}
