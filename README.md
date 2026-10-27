# ReTV Bridge

A small open-source helper for Windows, macOS and Linux that lets the ReTV website (https://retv.app) talk to an Android TV on your home network through Google's `adb`.

The browser cannot speak the adb protocol. The Bridge runs on your computer, listens only on `127.0.0.1`, and relays the commands the ReTV site sends to the TVs you have already authorised for adb.

- Windows and macOS: a tray app (Rust + Tauri).
- Linux: the command-line build, `cargo run -p retv-bridge-cli`.
- Licence: MIT.

## What it talks to

Only the addresses listed in [PRIVACY.md](PRIVACY.md): Google (platform-tools, once), retv.app (update check) and TVs on your local network. No analytics, no accounts.

## What the Bridge trusts

The Bridge is access to `adb shell`, not a sandbox. Know what it does and does not protect:

- **Whoever has the session token has full adb.** Any code that knows the token and runs on an allowed site gets a full adb shell on every device your computer has already authorised for adb: TVs, phones and other Android devices on the network, on any port. An XSS bug or a compromised script on an allowed site (including a third-party library on it) is therefore a real threat: such code can read the token from the tab and run anything on those devices.
- **`Origin` checks only stop other websites in the browser.** Any program on your computer can forge the `Origin` header. Against local processes only the token helps, and a process that can read your terminal window or browser memory can read the token too.
- **The `writeFile` restriction is not a security boundary.** `writeFile` writes only inside `/sdcard/ReTV/`, but `run` executes any shell command the adb user can. The restriction protects against path mistakes, not against an attacker.
- **Run the Bridge only while you use ReTV and quit it afterwards.** While it is not running, the site cannot do anything to your devices.

## Build from source

Requirements: a recent stable Rust toolchain.

```bash
cargo test --workspace
cargo run -p retv-bridge-cli        # Linux / headless
cargo install tauri-cli --locked
cargo tauri build                   # Windows / macOS tray app
```

Release builds are made by GitHub Actions from tags `bridge-v*` and are signed as described in [CODE_SIGNING_POLICY.md](CODE_SIGNING_POLICY.md).

`tauri.conf.json` asks for updater artifacts, so `cargo tauri build` needs the updater key (below). A local build without it:
`cargo tauri build --config '{"bundle":{"createUpdaterArtifacts":false}}'`.

## Updates

The tray app checks `https://retv.app/downloads/retv-bridge.json` and `https://ru.retv.app/downloads/retv-bridge.json` (the site of the system language first) at start and then daily, through the Tauri updater. An update is installed only when you click «New version … available», never while a TV session is open, and only if its minisign signature matches the public key in `app/tauri.conf.json` (`plugins.updater.pubkey`).

Until that key is set, the repository carries the placeholder `REPLACE_WITH_THE_RETV_BRIDGE_UPDATER_PUBLIC_KEY` and builds with it never check for updates. Setting up the key (the maintainer, once):

1. `cargo tauri signer generate -w ~/.tauri/retv-bridge.key` with a password. Keep the private key and the password in a password manager plus one offline backup; losing them means every user reinstalls by hand. Never commit the private key.
2. Put the contents of `~/.tauri/retv-bridge.key.pub` into `plugins.updater.pubkey`.
3. Add the CI secrets `TAURI_SIGNING_PRIVATE_KEY` (the private key file's contents) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.

Release gate: `RETV_RELEASE=1 cargo test -p retv-bridge-app release_has_updater_key` fails while `tauri.conf.json` still carries the placeholder (a script can also simply `grep -q REPLACE_WITH_THE_RETV_BRIDGE_UPDATER_PUBLIC_KEY app/tauri.conf.json`; the string is `PUBKEY_PLACEHOLDER` in `app/src/updates.rs`).

`plugins.updater.requireSignedVersion` is on: the signature's trusted comment must name the version the manifest announces, so a tampered manifest cannot pair a new version number with an older signed release. `tauri build` records the version itself; a file signed by hand (e.g. the Windows installer after Authenticode signing) needs `cargo tauri signer sign --app-version <version> <file>`.

Publishing: stage every release to both sites, `retv.app` and `ru.retv.app`, with the same version, files and signatures. Russian systems ask `ru.retv.app` first and only fall back to `retv.app` when it fails, so a manifest left behind on one site holds back that site's users.

Testing an update locally (debug builds only): generate a throwaway key outside the repository, build two versions with `cargo tauri build --debug --bundles app --config <file>` (the file sets `version` and `plugins.updater.pubkey`; `TAURI_SIGNING_PRIVATE_KEY` points at the throwaway key), serve a manifest naming the newer `.app.tar.gz` and its `.sig` with `python3 -m http.server`, and start the older build with `RETV_BRIDGE_UPDATE_URL=http://127.0.0.1:<port>/downloads/retv-bridge.json` (add `RETV_BRIDGE_UPDATE_AUTOINSTALL=1` to have it act as if the tray item were clicked). Release builds ignore both variables.

## Security

See [SECURITY.md](SECURITY.md).
