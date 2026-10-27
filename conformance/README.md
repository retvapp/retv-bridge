# Bridge conformance fixtures

Golden fixtures for the bridge protocol described in [`../PROTOCOL.md`](../PROTOCOL.md). Two runners replay
them:

- the TypeScript reference bridge, which lives in the ReTV monorepo and is not part of this repository (its
  runner is `test/conformance.test.ts` there);
- the Rust bridge: `crates/core/tests/conformance_{validators,pairing,wire}.rs`.

In this repository only the Rust runner runs: `cargo test -p retv-bridge-core --test conformance_validators
--test conformance_pairing --test conformance_wire` (or just `cargo test --workspace`).

The TypeScript bridge defines the protocol (v2 now: v1 plus `features`, `installApk` and `no-space`). When a
fixture and the TS bridge disagree, the fixture is wrong. A bridge that speaks only v1 fails `hello`'s
`protocol` / `features` and the `install-apk-*` / `no-space` fixtures.
A fixture is a contract: change one only together with `PROTOCOL.md`, and make both runners pass.

Files:

| File | What it drives |
| --- | --- |
| `validators.json` | the pure input checks |
| `pairing.json` | the site pairing state machine (`pairStart` / `pairFinish`) with a fake clock |
| `wire/*.json` | a running bridge over a real WebSocket, with a fake adb |
| `mdns/*.json` | captured mDNS answers (added by Phase 0, parsed by the Rust mDNS crate) |

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
| `decodeBase64` | any | the decoded bytes as lowercase hex, or `null` unless the input is canonical padded standard base64: no whitespace, no URL-safe alphabet, unused trailing bits zero (`YR==` is `null`) |
| `groupServices` | `[{ name, service, address }]`, adb's mDNS lines | the TVs, one per LAN IP in first-seen order: `{ name, ip, connectPort, pairPort }`. `name` is the first line's; `pairPort` from the last `_adb-tls-pairing.` line, `connectPort` from the first `_adb-tls-connect.` or `_adb.` line, else `null`. A line whose address does not `splitAddress` to a LAN IPv4 is skipped |

Lengths are counted in UTF-16 code units in TS; every vector uses ASCII, so bytes and characters agree.

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
{ "description": "...", "adb": {...}, "pairCodes": ["482917", ...], "steps": [...] }
```

For each file the runner:

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
- **run(cmd)** on a device records `[serial, cmd]` in `ran`, then: `shell[cmd]` with `error` fails with that
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
