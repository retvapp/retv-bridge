# Bridge conformance fixtures

Golden fixtures for the bridge protocol described in [`../PROTOCOL.md`](../PROTOCOL.md). Two runners replay
them:

- the TypeScript reference bridge, which lives in the ReTV monorepo and is not part of this repository (its
  runner is `test/conformance.test.ts` there);
- the Rust bridge: `crates/core/tests/conformance_{validators,commands,pairing,wire}.rs`.

ReTV Helper, the app on the TV that serves the same protocol through the ReTV relay (`PROTOCOL.md`, "Helper profile"),
runs `validators.json`, `commands.json` and the `helper` wire fixtures in process (see [Profiles](#profiles)). Its
runner lives in the ReTV monorepo and is not part of this repository.

In this repository only the Rust runner runs: `cargo test -p retv-bridge-core --test conformance_validators
--test conformance_commands --test conformance_pairing --test conformance_wire` (or just `cargo test --workspace`).

The TypeScript bridge defines the protocol (v3 now: v2 plus the `run` allow-list, feature `allowlist`, error
`command-not-allowed`). When a fixture and the TS bridge disagree, the fixture is wrong. A bridge that speaks only v2
fails `hello`'s `protocol` / `features`, `commands.json` and `wire/run-allowlist.json`; one that speaks only v1 also
fails the `install-apk-*` / `no-space` fixtures.
A fixture is a contract: change one only together with `PROTOCOL.md`, and make both runners pass.

Files:

| File | What it drives |
| --- | --- |
| `validators.json` | the pure input checks |
| `commands.json` | the `run` allow-list (protocol v3) |
| `pairing.json` | the site pairing state machine (`pairStart` / `pairFinish`) with a fake clock |
| `wire/*.json` | a running bridge over a real WebSocket, with a fake adb; the `helper` ones, ReTV Helper's dispatcher in process |
| `wire/run-per-user.json` | `run` with `--user N` commands: passed through unchanged, a SecurityException on stderr (exit 255) is an ordinary result |
| `mdns/*.json` | captured mDNS answers (added by Phase 0, parsed by the Rust mDNS crate) |
| `e2e/*.json` | the ReTV Helper end-to-end encryption (phone ↔ TV through the relay), not the bridge protocol |
| `relay/*.json` | the ReTV relay's rooms, link codes and per-IP limits, not the bridge protocol |

All fixtures are UTF-8 JSON. A top-level `"description"` (or a scenario's `"name"`) is for people; runners
ignore it.

## validators.json

```json
{ "<fn>": [[input, expected], ...] }
```

Each vector calls `fn(input)` and compares the output with `expected` by deep equality. `input` may be any JSON
value (a non-string where a string is expected must give the "no" answer, not an error).

| fn | input | expected |
| --- | --- | --- |
| `isLanIpv4` | any | `true` for dotted IPv4 (no leading zeros) in 10/8, 172.16/12, 192.168/16, 169.254/16 |
| `isPort` | any | `true` for a JSON number whose value is an integer 1..65535; the spelling does not matter (`5555.0` and `1e3` are integers), a string never is |
| `isAllowedDevicePath` | any | `true` for a normalised absolute path strictly below `/sdcard/ReTV/`, at most 512 characters, no NUL, CR or LF, no trailing `/` |
| `isAllowedBinaryPath` | any | `isAllowedDevicePath`, or exactly `/data/local/tmp/retv-home.apk` |
| `splitAddress` | string | `{ "ip", "port" }` split at the last `:`, or `null` unless the part after it is plain decimal digits without a leading zero, sign, space, `0x`, exponent or fraction, and 1..65535 |
| `isAllowedApkUrl` | any | `true` for a string that starts with `https://retv.app/downloads/` or `https://ru.retv.app/downloads/`, whose rest matches `^[A-Za-z0-9._/-]+\.apk$`, and that has no `..`, `?` or `#` anywhere (v2) |
| `isDevApkUrl` | any | `true` for a dev site's APK URL: `http://` + `localhost`, `127.0.0.1` or a LAN IPv4 (`isLanIpv4`), `:` + a plain decimal port 1..65535, then `/downloads/` and the `isAllowedApkUrl` path rule, no `..`, `?` or `#` (a dev bridge's `installApk` takes it: PROTOCOL.md "Development builds") |
| `decodeBase64` | any | the decoded bytes as lowercase hex, or `null` unless the input is canonical padded standard base64: no whitespace, no URL-safe alphabet, unused trailing bits zero (`YR==` is `null`) |
| `groupServices` | `[{ name, service, address }]`, adb's mDNS lines | the TVs, one per LAN IP in first-seen order: `{ name, ip, connectPort, pairPort }`. `name` is the first line's; `pairPort` from the last `_adb-tls-pairing.` line, `connectPort` from the first `_adb-tls-connect.` or `_adb.` line, else `null`. A line whose address does not `splitAddress` to a LAN IPv4 is skipped |

Lengths are counted in UTF-16 code units in TS; every vector uses ASCII, so bytes and characters agree.

## commands.json

```json
{ "description": "...", "vectors": [[cmd, expected], ...] }
```

Each vector calls `allowCommand(cmd)` (Rust `allow_command`) and compares the result with `expected`: the shell line the
bridge runs (rebuilt from the checked words, see PROTOCOL.md "Protocol v3"), or `null` for a command the bridge refuses
with `command-not-allowed`. A `cmd` that is not a string is refused (`null`), never an error.

## pairing.json

```json
[{ "name": "...", "token": "T", "codes": ["111111", ...], "disabledCalls": 1,
   "steps": [{ "at": 0, "op": "start" | "finish", "origin": "A", "code": "...", "expect": {...}, "shown": "111111" }] }]
```

Each scenario builds a fresh pairing state with:

- the session token `token`;
- a code source that returns `codes` in order (running out is a fixture error);
- a clock. Before each step the clock reads `T0 + at`, where `T0` is any fixed value (the TS runner uses
  1 000 000). `at` is in milliseconds and never decreases. Results must not depend on `T0`.

A step calls `start(origin)` or `finish(origin, code)` and compares the result with `expect` by deep equality.
Results are the PairStartResult / PairFinishResult shapes from `PROTOCOL.md`:
`{ "expiresIn" }`, `{ "token" }`, `{ "error": "bad-code", "attemptsLeft" }`,
`{ "error": "pair-locked" | "pair-busy", "retryIn" }`, `{ "error": "pair-expired" | "pair-disabled" }`.

`shown`: the code that this step showed to the person at the computer (the bridge window). Present: the step
showed exactly that one code, once. Absent: the step showed nothing.

`disabledCalls`: how many times the "pairing disabled, restart the bridge" notice fired over the whole
scenario. Absent means 0.

## wire/*.json

```json
{ "profiles": ["bridge"], "description": "...", "adb": {...}, "pairCodes": ["482917", ...], "steps": [...] }
```

For each file whose [profiles](#profiles) include `"bridge"`, the bridge runner:

1. builds a fake adb from `adb` (below);
2. starts the bridge on `127.0.0.1`, port 0 (any free port), with the session token `"test-token-123"`, a
   fake clock (starts anywhere, moves only on `advanceMs`), and a pairing code source that returns
   `pairCodes` in order (random codes after that, so a fixture must not rely on them);
3. runs `steps` in order, each to completion before the next;
4. at the end, fails if any open socket received a reply that no step read.

### `adb`: the fake adb

```json
{
  "available": true,
  "pairCode": "123456",
  "tvs": [{ "name": "adb-X", "ip": "192.168.1.7", "connectPort": 37123, "pairPort": null }],
  "devices": {
    "<ip:port>": {
      "model": "MiTV-AYFR0",
      "shell": { "<cmd>": { "stdout": "...", "stderr": "...", "exitCode": 0 }, "<cmd2>": { "error": "<text>" } },
      "connectError": "<text>",
      "writeError": "<text>"
    }
  },
  "http": { "<url>": { "status": 200, "bodyHex": "...", "repeat": 1, "redirect": "<url>" } }
}
```

Every field is optional. The fake behaves like this:

- **available**: whether the local adb server answers (`hello`'s `adb`). Default `true`.
- **discover** returns `tvs` (default `[]`).
- **pair(address, code)** succeeds when `code` equals `pairCode` (default `"123456"`) and records `address` in
  `paired`; otherwise it fails with `pairing-failed`.
- **connect(address)**:
  - an address not in `devices` fails with `device-unreachable`;
  - a device with `connectError` fails with that text as the error. A transport code (`unauthorized`, ...)
    must reach the site unchanged; any other text (a path, stderr, a stack trace) must be answered exactly
    `internal`;
  - otherwise it succeeds, records `address` in `connected`, and the device's serial is `address`.
- **The model** is `model` (absent: unknown, so the reply's `model` is `""`). How a bridge reads it is not
  part of the protocol: the fake answers `getprop ro.product.model` with `"<model>\n"` (exit 0, or exit 1
  without `model`) and does not record it in `ran`; a Rust fake may hand the model over with the connection
  instead.
- **run(cmd)** on a device records `[serial, cmd]` in `ran`, then (`cmd` is what the bridge ran: in v3 the line it
  rebuilt, so `shell` keys and `ran` hold rebuilt lines): `shell[cmd]` with `error` fails with that
  text (same rule as `connectError`); `shell[cmd]` with a result returns it; anything else returns
  `{ "stdout": "", "stderr": "", "exitCode": 127 }`.
- **writeFile(path, text)** and **writeBytes(path, bytes)** record `pushed[path] = "sha256:<hex>"` of the UTF-8
  text or of the bytes; with `writeError` on the device they fail with that text instead (same rule as
  `connectError`) and record nothing.
- **close()** records the serial in `closed` (a device replaced by a second `connect` to the same serial closes
  too).
- **http** (protocol v2, `installApk`): the bridge's downloader, faked per exact URL. The runner hands the
  bridge a fetcher that:
  - fails like a network error for a URL not in `http`;
  - for an entry with `redirect`: answers a redirect (`status`, default 302, `Location: redirect`). A downloader
    that refuses redirects (the protocol requires it; TS: `redirect: "error"`) fails, which the bridge answers
    `download-failed` with no result. (The TS fake follows the redirect only if asked to, so following one would
    fail the fixture.)
  - otherwise answers `status` (default 200) with a body of `bodyHex` repeated `repeat` times (default 1) and a
    matching `Content-Length`. `repeat` keeps the 64 MiB fixtures small. A 3xx answered this way (no `redirect`:
    a client that refuses redirects still receives it) must also be `download-failed` with no result. A Rust
    fetcher may report it as its status; the bridge drops the status of any 3xx.

The installApk fixtures expect exact commands in `ran`: `pm install -r --user current /data/local/tmp/retv-install.apk` and
`rm -f /data/local/tmp/retv-install.apk`, and the staged file in `pushed` at `/data/local/tmp/retv-install.apk`.

### Steps

Each step is one object. `"socket": n` picks the socket it acts on (default 0).

| Step | What the runner does |
| --- | --- |
| `{ "open": { "origin", "host" }, "expectStatus": 101 \| 403 }` | Opens `ws://127.0.0.1:<port>` with that `Origin` header (`"origin": null`: no Origin header) and that `Host` header (default `"$host"`). Checks the upgrade status. On 101 the socket becomes socket `n`, replacing any earlier one. |
| `{ "send": {...}, "expect": {...} }` | Sends the object as one JSON text frame, after replacing every `"$token"` value with the session token and every `"$code"` value with the last code the bridge showed. Reads the next reply on that socket and matches it against `expect`. |
| `{ "sendRaw": "<text>", "expect": {...} }` | Sends the string as-is as one text frame (malformed JSON), then reads and matches the next reply. |
| `{ "sendBinary": "<hex>", "expect": {...} }` | Sends those bytes as one binary frame, then reads and matches the next reply. |
| `{ "httpGet": "/", "expectStatus": 426 }` | A plain HTTP GET (no upgrade) to the bridge port; checks the status. |
| `{ "sendOversize": n, "expectClose": true, "closeCode": 1009 }` | Sends one text frame of `n` bytes of `x` and waits for the bridge to close the socket. |
| `{ "sendOversize": n, "expect": {...} }` | The same frame, but reads and matches a reply (a frame at the size limit is still read: it is not JSON, so `bad-request`). |
| `{ "expectClose": true, "closeCode": 1008 }` | Waits for the bridge to close the socket; `closeCode` (optional) is the WebSocket close code. |
| `{ "advanceMs": n }` | Moves the fake clock forward by `n` ms. |
| `{ "close": true }` | The client closes the socket and waits for the close to finish. |
| `{ "expectAdb": { "connected", "closed", "paired", "pushed", "ran" } }` | Compares each given key of the fake adb's record with the expected value. `connected`, `paired` and `ran` are in call order; `closed` is compared as a multiset (sorted); `pushed` is the whole map. Device closes after a socket closes are asynchronous: the runner retries for up to 2 s before failing. |

Waits (a reply, a close) time out after 2 s.

Inside `send`, an object `{ "$bytes": { "hex": "<hex>", "repeat": n } }` (any depth) is replaced by the base64
of those bytes repeated `n` times (default 1). It keeps large uploads out of the fixture files.

Host strings: `"$host"` is `127.0.0.1:<port>`; `$port` inside a string is the bridge's port, e.g.
`"evil.example:$port"` or `"localhost:$port"`.

### Matching `expect`

Deep equality, strict: an object must have exactly the expected keys (no extra ones), arrays match element by
element, numbers and strings by value. Inside `expect`:

- `"$any"`: the key is present, any value (used for `hello`'s `version`, which differs between bridges);
- `"$token"`: equals the session token `"test-token-123"`;
- `"$code"`: equals the last code the bridge showed.

Replies keep the request's `id`; a message without a usable id is answered with `"id": -1`.

### Profiles

A wire fixture's top-level `"profiles"` lists who runs it: `"bridge"` (the TS and Rust bridges) and/or `"helper"`
(ReTV Helper, `PROTOCOL.md` "Helper profile"). Absent means `["bridge"]`; every file states it anyway. Any other value
(not a non-empty list of those two names) is a fixture error.

- `["bridge"]`: what only a bridge has: Origin / Host, the session token (`bad-token`), `pairStart` / `pairFinish`, LAN
  `connect` / `pair`, `discover` over mDNS, several sockets.
- `["helper"]` (`helper-*.json`): what only the Helper has (`helperInfo`, `adbStart`, `adbPair`, `endSession`, `connect`
  to the TV itself, its `hello`), and the Helper's twins of bridge scenarios whose TV address or sockets matter.
- `["bridge", "helper"]`: `run` (the allow-list), `writeFile`, `writeBytes`, `installApk` and `no-space`, the same
  contract in both. These fixtures name their one TV with two placeholders, replaced before the run as exact string
  values and as object keys (in `adb.devices` too):

  | placeholder | bridge | helper |
  | --- | --- | --- |
  | `"$tvIp"` | `"192.168.1.5"` | `"127.0.0.1"` |
  | `"$tvSerial"` | `"192.168.1.5:5555"` | `"self"` |

  Their `connect` sends no `port`, so the bridge takes 5555 (the fake announces no mDNS port). Every `send` keeps the
  `"$token"` placeholder, which the bridge checks and the Helper ignores. They have no step only one side can run (no
  wrong token, no second socket, no `hello`).

### The helper runner

ReTV Helper runs the `helper` fixtures **in process**: no socket, no relay, no E2E, no token.

- `open` steps are skipped. A `helper` fixture has no `socket` other than 0, `httpGet`, `close` or `advanceMs` step.
- `send` (after the `$token` / `$code` / `$bytes` expansion above), `sendRaw` and `sendOversize` hand that text to
  `Dispatcher.handle(text)`; its reply is matched against `expect` as above. `sendBinary` does not occur.
- `expectClose` means the dispatcher returned "close": alone, for the `send` before it (`endSession`, whose reply is
  still sent first); with `sendOversize`, for that text (over 1 MiB, no reply). `closeCode` is not checked. No `send` follows it in the same fixture, but `expectAdb` may.
- `expectAdb` reads the helper fake's record (below).
- A shared fixture (`["bridge", "helper"]`) begins with `connect`, which on the Helper needs the session's adb first
  (`adb-not-ready`). So before its steps the runner sends `{"type": "adbStart", "method": "classic"}` (with its own `id`)
  and expects `{"method": "classic"}`; the fake's defaults allow it, and it records nothing. `helper-*.json` fixtures
  start the session's adb themselves.

The helper fake is built from the same `adb` object. `devices` has one entry, `"self"` (`$tvSerial`), with the same
`model`, `shell`, `writeError` meaning as for the bridge; `http` and `pairCode` are as for the bridge. `adb.helper`
describes the TV's own adb:

```json
"helper": { "sdk": 30, "release": "11", "classicPort": true, "rsa": "allow", "pairingService": true,
            "wirelessConnect": true, "wirelessPaired": false }
```

| field | default | meaning |
| --- | --- | --- |
| `sdk`, `release` | `30`, `"11"` | `helperInfo`'s `sdk` and `release` (the model is `devices.self.model`, `""` when absent) |
| `classicPort` | `true` | `127.0.0.1:5555` answers. `false`: `adbStart classic` is `device-unreachable` |
| `rsa` | `"allow"` | the RSA dialog: `"allow"` connects; `"deny"` and `"timeout"` (no answer in 60 s) are `unauthorized` |
| `pairingService` | `true` | an `_adb-tls-pairing` service appears. `false`: `adbPair` is `no-pairing-service` |
| `wirelessConnect` | `true` | an `_adb-tls-connect` service appears. `false`: wireless `adbStart` (and `adbPair`'s connect) is `wireless-off` |
| `wirelessPaired` | `false` | our key is already paired with the TV. `false`: `adbStart wireless` is `pairing-needed` until an `adbPair` succeeds |

`adbPair` succeeds when `code` equals `pairCode`, records `"self"` in `paired` and sets `wirelessPaired`, else it is
`pairing-failed`. `connect` records `"self"` in `connected`, `disconnect` records it in `closed`. `run`, `writeFile`,
`writeBytes` and `installApk` record `ran` and `pushed` as for the bridge, with serial `"self"`. `endSession` records
its command in `ran`: after a wireless session always `settings put global adb_wifi_enabled 0`, with `"off"` or
`"keep"` (every Helper session ends with Wireless debugging off); after a classic session
`settings put global adb_enabled 0` with `"off"`, and nothing with `"keep"`. The reply's `debugging` is the truth:
`"off"` when debugging is now off (so `"keep"` after a wireless session answers `"off"`), `"kept"` otherwise.

## mdns/*.json

Captured mDNS answers from real TVs, one per file, for the Rust mDNS parser:

```json
{ "packetHex": "...", "from": "192.0.2.10",
  "expect": [{ "instance": "...", "kind": "connect", "ip": "...", "port": 0 }] }
```

`packetHex` is the raw UDP payload as hex, `from` the sender address, and `expect` the services the parser
must find in it, in order. The TS bridge asks the adb server for mDNS and does not run these. Published
fixtures and docs use neutral addresses: documentation examples use TEST-NET-1 (`192.0.2.x`), and fixtures
that need a home-network address use `192.168.1.x` / `10.x`, never a real device's address.

## e2e/*.json

The end-to-end encryption between the phone (`ctl`) and ReTV Helper on the TV (`tv`) through the relay. It is not
part of the bridge protocol and does not change `PROTOCOL.md`. The TypeScript reference is the web app's
`wizard/relay/e2e.ts` in the ReTV monorepo (runner: `relay-e2e.test.ts` there), and the Helper's Kotlin must produce
the same bytes. Hex is lowercase, text is UTF-8, and base64url has no padding.

Sizes are fixed, and any other size is refused (`e2e-bad-key` for an input, `e2e-bad-hello` inside a message): keys
32 bytes, nonces 16, `roomId` 16, `k` 32, `p` (pubHash) 16, the commitment 32.

### The handshake

Every hello carries a fresh 16-byte nonce from its sender. The relay tells these text frames from its own because
`"e2e"` is the first key.

- **QR mode** (the phone has `k` and `p` from the QR):
  1. `ctl → tv {"e2e":"hi","v":1,"mode":"qr","pub":ctlPub,"nonce":ctlNonce}`
  2. `tv → ctl {"e2e":"hi","v":1,"pub":tvPub,"nonce":tvNonce}`; the phone checks `pubHash(tvPub) == p`
     (`e2e-bad-pub-hash`) before it seals anything.
- **Code mode** (commit and reveal):
  1. `ctl → tv {"e2e":"hi","v":1,"mode":"code","commit":C}`, `C = SHA-256("retv-e2e-commit" ‖ ctlPub ‖ ctlNonce)`
  2. `tv → ctl {"e2e":"hi","v":1,"pub":tvPub,"nonce":tvNonce}`
  3. `ctl → tv {"e2e":"reveal","v":1,"pub":ctlPub,"nonce":ctlNonce}`; the TV checks `C` (`e2e-bad-commit`).

  The relay must commit to its key before it sees the TV's nonce, and the phone's nonce stays hidden until the TV has
  committed to its own. So a relay in the middle cannot grind the SAS: each code-mode attempt is one guess with
  probability 10^-6. With the attempt limit below, that is at most 3 guesses per action of the person at the TV.
- The TV accepts only the mode it displays at that moment and refuses a hello for the other mode
  (`e2e-mode-mismatch`). The start screen shows the QR only, so the TV expects QR mode. The link code appears only
  after «Нет камеры? Показать код», which hides the QR and switches the TV to code mode. «Показать QR» switches back and
  voids the code.
  Messages have exactly the keys above, `v` = 1. A second hello, a reveal out of turn, or any text after a failure is
  `e2e-bad-hello`, and a failed handshake stays failed.
- `pubHash(pub)` = the first 16 bytes of `SHA-256(pub)`: the `p` in the QR link.

TV rules, which the vectors cannot show:

- A handshake is **complete** when the TV has opened the phone's first confirm frame. A TV keypair serves exactly one
  completed handshake. Only then does the TV make a new keypair, and with it a new QR and code. The TV's first sealed
  frame is `{"t":"confirm","next":p'}`, where `p'` is the pubHash of the next keypair, so a QR phone can re-handshake
  after a dropped socket without scanning again.
- In code mode the TV shows the SAS and dispatches nothing before «Разрешить».
- **Failed code-mode attempts:**
  - An attempt counts as failed once the TV has sent its hello, unless it ends in «Разрешить». That covers a missing or
    bad reveal, a timeout, a socket drop, `peer-left` and «Не совпадает».
  - After 3 failed attempts the TV stops accepting code-mode hellos until the person presses «Новый код» on the TV.
    Nothing resets or rotates by itself.
  - The phone never re-handshakes in code mode on its own. It shows a new SAS only when the person explicitly retries.

### The key schedule

- `shared = X25519(myPriv, theirPub)`. An all-zero `shared` is refused (`e2e-bad-key`).
- `ikm = shared ‖ k` in QR mode, `ikm = shared` in code mode.
- `salt = SHA-256("retv-helper-v1" ‖ roomId ‖ tvNonce ‖ ctlNonce)`.
- `modeByte` = `0x01` QR, `0x02` code.
- `okm = HKDF-SHA256(ikm, salt, info = "retv-helper-v1 keys" ‖ modeByte ‖ tvPub ‖ ctlPub, 64)`; `kCtlToTv = okm[0..32]`,
  `kTvToCtl = okm[32..64]`.
- `sas = HKDF-SHA256(ikm, salt, "retv-helper-v1 sas" ‖ modeByte ‖ tvPub ‖ ctlPub, 8)`, read as a big-endian unsigned
  64-bit integer, mod 1 000 000, zero-padded to **6 digits**. It covers `shared`, both public keys and both nonces.

### Frames

- After the handshake, binary frames only: `counter (8 bytes BE) ‖ AES-256-GCM(key, nonce = dir ‖ counter, aad =
  roomId, plain)`, with the 16-byte tag at the end. `dir` is 4 bytes BE: `1` ctl→tv, `2` tv→ctl.
- The first sealed frame each way is a confirm, with exactly these keys: `{"t":"confirm"}` from the phone, and
  `{"t":"confirm","next":"<22-char base64url>"}` from the TV (16 bytes, canonical, no padding). A peer that fails to
  open it, or to parse it as the other side's confirm (`e2e-bad-confirm`), closes with `e2e-failed`.
- Each direction counts from 0, and a receiver accepts exactly the next counter. A replay, gap, reorder, bad tag or
  short frame is `e2e-bad-frame`, and it **latches**: every later `open` and `seal` on that session fails too. A TV
  that gets one on a live session ends the session (debugging off, as «Отключить телефон»); it does not wait for the
  phone to re-handshake.
- A sender stops with `e2e-counter-exhausted` rather than wrap past `2^64 - 1`.

### Files

`handshake.json`:

- `cases[]`, one for QR mode and one for code mode. Each has:
  - the inputs: `roomIdHex`, `kHex` (the QR `k`, or `null`), both key pairs and both nonces;
  - the exact message texts: `ctlHello`, `tvHello`, and `ctlReveal` (`null` in QR mode);
  - every intermediate value: `tvPubHashHex`, `commitHex`, `sharedHex`, `ikmHex`, `saltHex`, `keysInfoHex`,
    `sasInfoHex`, `okmHex`, `kCtlToTvHex`, `kTvToCtlHex`, `sasOkmHex`, `sas`.

  The keys are RFC 7748 §6.1's (tv = Alice, ctl = Bob), so `sharedHex` is that RFC's `K`. With the case's keys and
  nonces injected, a runner must produce each message text exactly.
- `reject[]`: `{ name, case, side, feed: [text...], error }`. The runner builds that side for the case:
  - the TV, with the case's mode, `k`, TV keys and TV nonce;
  - or the phone, with its keys, nonce and `p`.

  It feeds every text but the last, and each must succeed. The last must fail with `error`.

`frames.json`:

- `cases[]`, by the same `name`: `ctlToTv` and `tvToCtl`, the first three frames each way (`plainUtf8`,
  `frameHex`). A runner seals each `plainUtf8` in order and must get `frameHex`, and opens each `frameHex` in order and
  must get `plainUtf8`.
- `reject[]`: `{ name, case, dir, steps: [{ frameHex, ok }] }`. The runner builds the receiving side of `dir` for that
  case (`ctlToTv`: the TV) and opens each step in order. A step must succeed when `ok` is true, and fail with
  `e2e-bad-frame` when it is false. The `latch-*` scenarios check that a session stays dead.
- `confirm.accept[]`: `{ from, plainUtf8, nextHex }`. The confirm parser for `from` must accept `plainUtf8` and give
  `nextHex` (`null` for the phone), and the builder must produce `plainUtf8` exactly.
- `confirm.reject[]`: `{ name, from, plainHex }`. The parser for `from` must refuse those bytes (`e2e-bad-confirm`).
  The bytes are hex because one case is not valid UTF-8.

## relay/*.json

The ReTV relay's pure logic: rooms, link codes and per-IP limits, with a fake clock. Like `e2e/*.json` it is not part
of the bridge protocol. The reference is the ReTV server's `relay/{rooms,codes,limits}.ts` in the ReTV monorepo
(runner: `relay/logic.test.ts` there; the WebSocket server around it is `relay/server.ts`, see [The relay's
wire](#the-relays-wire)).

The relay pairs two sockets and passes their frames on. It sees only ciphertext, room ids and link codes, never `k`,
`p`, a key, a nonce or the SAS, and it logs no IP, code or content. Its rules never weaken the TV's E2E rules above:

- A room id is 16 random bytes in base64url (`^[A-Za-z0-9_-]{22}$`), checked before anything else (`bad-room-id`).
  The `tv` creates the room (`full` at `maxRooms`); a `ctl` needs an existing one (`no-room`). Each role has one
  slot, and a second peer for it is `busy`. A peer that leaves frees its slot and the other peer is told; the last
  one out deletes the room. A free `ctl` slot is open to anyone with the room id, which is safe because the TV's E2E
  handshake, not the relay, decides who it talks to.
- A room expires `idleMs` (30 min) after its last activity (an `open`, or a `touch` per frame), or `maxLifeMs` (4 h)
  after it was created, whichever comes first. Rejoining does not renew its life. An expired room is gone at once, even
  before a `sweep` removes it: `has` is false, `touch` does not renew it, `forward` gives `null`, `open` never
  resurrects it (a `ctl` gets `no-room`), and the `full` check expires such rooms first. The next `sweep` still reports
  each of them once, before the rooms it expires itself.
- **A TV alone** (with `aloneMs` and `aloneMaxMs` set; the server sets 10 min and 30 min, off by default in the logic):
  a room no `ctl` has ever joined also expires `aloneMs` after its creation or the TV's last code claim (`claimed`),
  and `aloneMaxMs` after its creation at most. The TV's own frames do not renew it. So a TV showing a QR or a code
  keeps its room about as long as a code lives, and a socket that only holds a room cannot keep it for hours; the
  Helper then opens a fresh room with a new QR (it already does so after 10 unused minutes).
- A link code is **6 random digits drawn fresh at every claim** (`crypto.randomInt`), never derived from the room
  or reused on purpose. The TV claims one only when it shows the code («Нет камеры? Показать код», and again on
  «Новый код»), so the code never appears with the QR. It lives 600 s and works once: a hit gives the room id and
  consumes the code. A room has one live code; a new claim voids the old one, and `revoke` voids it when the TV goes
  back to the QR or the room ends. The code only finds the room: the phone still needs the commit and reveal, the SAS
  and «Разрешить» on the TV, and the TV's limit of 3 failed attempts until «Новый код» holds whatever the relay does.
- 5 lookup misses from one IP within 10 min block that IP for 10 min: the 5th miss still answers `not-found`, and
  every lookup in the block answers `rate-limited`, a right code included, without extending it. A malformed or
  expired code is a miss. 30 misses from one prefix (an IPv4 /24, an IPv6 /56) block that prefix the same way.
- **Per-code miss budget** (it replaced a global block that let a few dozen IPs stop every code lookup): a code is
  burned once `missBudget` misses (5000 on the relay) happened over every client while it was live. A burned code
  finds nothing (`not-found`); the next `sweep` names its room once in `burned`, and the server sends that TV
  `{"relay":"code-void"}`, on which the Helper claims a fresh code. Nobody is blocked, and a code claimed later counts
  only the misses after its claim. A guess at one live code within the budget wins with a chance of at most 0.5 %, and
  a hit still meets the commit and reveal, the SAS and «Разрешить» on the TV.
- Per IP: at most 20 room creations per sliding hour (a refused one is not counted), 8 open sockets, and 10 link-code
  claims per sliding 10 min (a refused one is not counted; a person presses «Новый код» far less often). Per prefix
  (/24, /56): 60 room creations per hour, 32 open sockets and 30 claims per 10 min. A refused event counts against
  neither the IP nor the prefix.
- Limits key on `ipKey = HMAC-SHA256(per-process random key, ipKeyInput(ip))`, held in memory only. The IP itself is
  never stored. `ipKeyInput` keeps an IPv4 address exact (an IPv4-mapped `::ffff:a.b.c.d` is that IPv4 address) and
  takes an IPv6 address as its /64 prefix in one spelling (`2001:db8:0:1::/64`), since one subscriber usually holds a
  whole /64 and could otherwise spread over 2^64 keys. The prefix key is the same HMAC over `prefix:` and
  `ipPrefixInput(ip)`: `198.51.100.0/24`, `2001:db8:0:100::/56`.

Expiry and windows are half-open: a code claimed at `t` is live while `now < t + 600 000`; a room expires when
`now - lastActive >= idleMs` or `now - createdAt >= maxLifeMs`; a miss, a creation or a claim counts while
`now - at < window`.

A file is one scenario:

```json
{ "description": "...", "options": { "maxRooms": 2 },
  "steps": [{ "at": 0, "op": "open", "room": "RoomOneAAAAAAAAAAAAAAA", "role": "tv", "peer": "tv", "expect": "ok" }] }
```

`options` (optional) overrides `createRooms`'s `idleMs`, `maxLifeMs`, `maxRooms` (4000 by default), `aloneMs` or
`aloneMaxMs`, and `createCodes`'s `missBudget`. A runner builds one rooms, one codes and one limits instance and one
client-keys function per file, sets the clock to each step's `at` (ms, never
decreasing), runs the step and compares its result with `expect` (deep equality) when the step has one:

| `op` | Arguments | `expect` |
| --- | --- | --- |
| `open` | `room`, `role` (`tv`/`ctl`), `peer` | `"ok"`, `"no-room"`, `"busy"`, `"full"` or `"bad-room-id"` |
| `leave` | `room`, `peer` | `{ "notify": peer or null }` |
| `touch` | `room` | none |
| `claimed` | `room` | none (the TV claimed a code: renews a TV alone) |
| `has` | `room` | `true` while the room exists and has not expired |
| `forward` | `room`, `from` | the other peer, or `null` |
| `sweep` | none | `{ "expired": [{ "roomId", "peers" }] }`, rooms in creation order, peers tv first; also sweeps codes and limits. In a file whose sweeps expect `burned`, the codes' `{ "burned": [roomId] }` instead |
| `claim` | `room`, `random`: the values `random6` returns, in order (all must be used) | `{ "code", "expiresIn": 600 }` |
| `revoke` | `room` | none |
| `lookup` | `code`, `ip` | `{ "roomId" }`, `"not-found"` or `"rate-limited"` |
| `create` | `ip` | `allowRoomCreate`: `true` or `false` |
| `claimAllowed` | `ip` | `allowCodeClaim`: `true` or `false` |
| `socket` | `ip`, `action`: `"open"` (`allowSocket`, expects a boolean) or `"release"` (`releaseSocket`) | as stated |

Peers are opaque names. `ip` goes through the runner's client-keys function first, so the logic only ever sees an
ipKey and a prefix key.
The IPs are documentation addresses (`198.51.100.x`, `203.0.113.x`, `2001:db8::/32`).

### The relay's wire

Not a fixture: what the TV and the phone see of the relay (`relay/server.ts`, tested by `relay/server.test.ts`).

- `GET /relay/v1/room/<roomId>?role=tv|ctl` upgrades to a WebSocket. Two caps refuse before the handshake, with a
  plain HTTP answer: `429` past 8 open sockets per IP or 32 per prefix, `503` past the relay's open sockets over all clients (4000,
  `RELAY_MAX_SOCKETS`). Any other refusal upgrades, then closes with:
  `4400` bad room id, role or message, `4404` no room (a `ctl`), `4409` busy, `4411` replaced (see the claim token
  below), `4429` rate-limited (20 new rooms per
  IP, 60 per prefix per hour; a `tv` joining its existing room is not a new room), `4503` full (4000 rooms), `4408`
  idle, max life, or a TV alone past its wait, `4410` the other peer left and did not come back within 10 min (checked by the 30 s sweep, so 10 to 10.5 min;
  the room then ends), `4413` the other peer is not reading: a frame that would take its unsent queue past 4 MiB
  closes the sender and is dropped (so a room never buffers more than 4 MiB per direction). A frame over 1 MiB + 1 KiB
  closes `1009`. Once the relay closes a peer, it ignores that peer's later frames, and the other peer gets `peer-left`
  at once.
- A `ctl` may offer the WebSocket subprotocol `retv.ctl.<token>` (16 random bytes, base64url), which the relay selects.
  The browser keeps the token with its link. A `ctl` whose token equals the token of the `ctl` holding the slot takes
  the slot over: the older socket closes `4411`, the `tv` gets `peer-left` then `peer-joined`. Another token, no token,
  or a holder that offered none still meets `4409`. The token grants nothing else: the `tv` still needs the E2E
  handshake (QR secret, or the check digits and «Разрешить») from the new socket. The token is no secret against the
  relay host or a proxy that logs request headers (`Sec-WebSocket-Protocol`): keep header logging off for `/relay/*`.
- Every 30 s the relay pings each peer; one that has not answered the previous ping (a half-open socket) is dropped,
  so its slot frees within about a minute and a phone can rejoin.
- Just before the `tv` gets `peer-joined` (or, on a take-over, the second `peer-joined`), it gets
  `{"relay":"ctl-id","id":"<16 bytes base64url>"}` when the `ctl` offered a claim token. The id is the first 16 bytes of
  SHA-256(`retv.ctl-id:` ‖ token). The Helper keeps the id of the phone its live session last completed with: that
  phone (the same browser tab) pairs again from the away screen as before, while any other phone that pairs from the
  away screen, QR included, waits for «Разрешить» on the TV with the check digits on both screens. An older relay sends
  no id, so every away-screen pairing then waits for «Разрешить»; an older Helper ignores the message. The id comes
  from the relay, so it only decides between asking and not asking a person who can already see the TV's QR.
- When a peer joins and the other is there, both get `{"relay":"peer-joined"}`. When a peer leaves, the other gets
  `{"relay":"peer-left"}`; the slot stays open for 10 min (a phone locked mid-setup comes back to the same room).
- Binary frames, and text frames that start with `{"e2e"`, go to the other peer exactly as sent (dropped while it is
  away). The only other text frames allowed are the `tv`'s `{"relay":"claim-code"}`, answered with
  `{"relay":"code","code":"123456","expiresIn":600}` (or `{"relay":"rate-limited"}` past 10 claims per 10 min; the
  socket stays open), and `{"relay":"void-code"}` («Показать QR»), which voids the room's code without an answer.
  Anything else closes `4400`. A room's code is also voided when the room ends. The relay may send the `tv`
  `{"relay":"code-void"}` unasked: its code was burned (the miss budget above), and a fresh claim gives a new one.
- `GET /relay/v1/code/<code>` answers `200 {"roomId"}`, `404 {"error":"not-found"}` or `429 {"error":"rate-limited"}`,
  always with `Cache-Control: no-store`. `GET /relay/healthz` answers `200 {"ok":true}`.
- The client IP is the rightmost `X-Forwarded-For` entry only on a connection from a trusted proxy
  (`RELAY_TRUSTED_PROXIES`, IPs or CIDR ranges; loopback by default, so a proxy in a docker container needs its bridge
  address there); otherwise it is the socket address. A request target that does not parse answers `400`, or drops
  the connection on an upgrade.
