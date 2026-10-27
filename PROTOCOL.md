# ReTV Bridge protocol

The ReTV site (the ReTV engine running in the browser) drives a real TV through the bridge: a small server on
the person's computer that runs the site's commands on the TV with adb. This document is the wire contract
between the site and any bridge (the TypeScript reference bridge in the ReTV monorepo, the Rust bridge in
this repository). The golden fixtures in [`conformance/`](conformance/README.md) pin it down; both bridges
must pass them.

- **Protocol v1**: below. The TypeScript bridge 0.1.x defines it.
- **Protocol v2**: v1 plus `hello`'s `features`, the `installApk` request and the `no-space` error code; see
  [Protocol v2](#protocol-v2) at the end. The current TypeScript bridge speaks v2. The site accepts a bridge on
  protocol 1 or 2.

## Transport

- WebSocket on `ws://127.0.0.1:5577`. The bridge binds the loopback interface only; it is not reachable from
  the network. Plain HTTP requests get `426` (WebSocket only), an upgrade with another method than `GET` gets
  `400`, and a request head over 16 KiB gets `431`.
- One JSON object per text frame, in both directions.
- Messages are capped at **1 MiB** (1 048 576 bytes). A bigger frame closes the socket with close code
  **1009** (message too big). A frame of exactly 1 MiB is still read.

## Connection checks

The WebSocket upgrade is refused with **403** before any message unless both hold:

- `Origin` is exactly one of:
  `http://127.0.0.1:5180`, `http://localhost:5180` (site dev server), `http://127.0.0.1:4189` (site tests),
  `https://retv.app`, `https://ru.retv.app`.
  The match is exact and case-sensitive: a missing `Origin`, `null`, another scheme, port, host, letter case
  or a trailing `/` are all refused.
- `Host` is exactly `127.0.0.1:<port>` or `localhost:<port>` (case-sensitive) for the port the bridge listens
  on (guard against DNS rebinding).

Refused connections never touch pairing state.

The bridge holds at most **16** sockets at once. Past the checks above, one more upgrade is refused with **503**
(Service Unavailable) until a socket closes.

## Messages

Request: `{ "id", "token", "type", ...params }`.

- `id`: a string of at most 64 characters or a finite number; the reply carries the same `id`.
- `token`: the session token. Required on every request except `hello`, `pairStart` and `pairFinish`.
- `type`: one of the request types below.

Reply: `{ "id", "ok": true, "result": {...} }` or `{ "id", "ok": false, "error": "<code>" }`. A few errors
carry details in `result` (see the error table). Error replies have no other keys.

The bridge handles each frame in this order:

1. A binary frame is never parsed (even if it holds JSON): `{ "id": -1, "ok": false, "error": "bad-request" }`.
2. A text frame that is not a JSON object, or has no usable `id`: the same `id: -1` reply. This comes before
   the token check, so an id-less request without a token is `bad-request`, not `bad-token`, and the socket
   stays open.
3. `hello`, `pairStart`, `pairFinish`: answered without a token check (below).
4. Every other type: the token check (`bad-token`, socket closes), then the request itself.

`hello` and the token-checked requests count against the socket's limit of **32** requests running at once: one
more is answered `too-many-requests` (nothing else happens) until one of them finishes.

Requests on one socket are taken in arrival order: each request's checks and its change to the socket's state
(the serials it holds, its `writeBytes` upload, its running install) happen before the next frame is read, so
a client may send requests back to back without waiting for replies (`writeBytes` chunks, `disconnect` then
`run`, a second `installApk` that is `busy`). Only their I/O with the TV runs side by side, and replies come as
each finishes, so match them by `id`.

### The session token

The bridge makes a random token (120 bits) at start; it changes on every run. The site gets it either from the
link the bridge opens (in the URL fragment, `#bridgeToken=...`, so it never reaches the site's server) or by
pairing with a code (below).

- `hello` without a valid token answers `paired: false` and keeps the socket open: this is how the site learns
  it needs the code.
- `pairStart` and `pairFinish` ignore `token` entirely: whatever it holds (absent, stale, valid, not a string)
  it has no effect on pairing and never causes `bad-token`. A site with a stale token can always re-pair.
- Any other request (except `pairStart` / `pairFinish`) without the valid token, including an unknown `type`,
  is answered `bad-token` and the bridge closes the socket with close code **1008** (policy violation). The
  check comes before anything else: nothing reaches adb.
- The token is compared in constant time and never logged.
- Opening the link puts it, token included, on a process's command line for a moment: `/usr/bin/open` on macOS
  (it exits within milliseconds; the browser gets the URL from LaunchServices, not as an argument), `xdg-open` on
  Linux, and on Windows the browser started for the link, whose command line other standard accounts cannot
  read. This is accepted: the token is only good against 127.0.0.1 on that computer, it changes on every start,
  and a program running as the same account can reach the bridge anyway. Calling LaunchServices directly would
  remove the macOS moment at the cost of unsafe native code; it was not worth it.

## Requests

| type | params | result |
| --- | --- | --- |
| `hello` | `version` (optional); token optional | `{ version, protocol: 1, adb, paired }` |
| `pairStart` | none; no token | `{ expiresIn }` |
| `pairFinish` | `code`; no token | `{ token }` |
| `discover` | none | `{ tvs: [{ name, ip, connectPort, pairPort }] }` |
| `pair` | `ip`, `port`, `code` | `{}` |
| `connect` | `ip`, `port` (optional) | `{ serial, model }` |
| `run` | `serial`, `cmd` | `{ exitCode, stdout, stderr }` |
| `writeFile` | `serial`, `path`, `data` | `{}` |
| `writeBytes` | `serial`, `path`, `offset`, `total`, `data` | `{ received, done }` |
| `disconnect` | `serial` | `{}` |

Any other `type` with a valid token is `bad-request`.

### hello

`version` is the bridge's own version string (it differs between bridges; clients must not depend on it),
`protocol` the protocol version (`1`). `adb` is `false` when the local adb server does not answer. `paired`
tells whether the request's `token` is the session token. If `protocol` is not the one the site expects, the
site asks the person to update the bridge.

### pairStart / pairFinish: pairing the site with a code

When the site has no token (it was opened before the bridge, in another browser, or the bridge was started
without opening a link), it asks for a code. The bridge shows 6 digits in its own window (and a desktop
notification), the person types them on the site, and `pairFinish` returns the session token.

There is one pairing state per bridge run, shared by all sockets; reconnecting does not reset it.

- The code is 6 digits from a CSPRNG. It lives **120 s** and is single-use.
- It is bound to the Origin that started it. While it is live, another Origin gets `pair-busy` with
  `retryIn` (seconds until the code expires) for both `pairStart` and `pairFinish`; its digits are never
  checked and never count as a miss, so it can neither guess nor burn the code.
- `pairStart` from the same Origin while the code is live shows the same code again with the same expiry,
  keeps the misses, and answers the remaining `expiresIn`.
- A wrong `pairFinish` answers `bad-code` with `attemptsLeft`. Any string that is not exactly the code is a
  miss, including malformed ones. The **5th** miss burns the code and locks both `pairStart` and `pairFinish`:
  `pair-locked` with `retryIn`. The first lock is **30 s**; each further lock doubles, up to **1 h**. The lock
  count lasts the whole run: a successful pairing does not reset it (a lock after a success is 60 s if one
  lock came before). The miss count belongs to one code and starts at 0 with each new code.
- `pairFinish` with no live code (never started, used, expired or burned) is `pair-expired`.
- One run issues at most **20** codes. After that `pairStart` is `pair-disabled` for every Origin until the
  bridge restarts; the bridge window says so. Re-showing a live code does not count as a new one.
- `code` must be a string of at most 16 characters, else `bad-request` (not a miss).
- `expiresIn` and `retryIn` are whole seconds rounded up, at least 1.
- Neither the code nor the token is ever logged.

Precedence, first match wins (a live code is one that is issued, unused, unburned and not expired):

- `pairStart`: 1. a lock in force: `pair-locked`; 2. a live code: from another Origin `pair-busy`, from the
  same Origin re-show it (`expiresIn`); 3. 20 codes issued already: `pair-disabled`; 4. issue a new code.
- `pairFinish`: 1. a lock in force: `pair-locked`; 2. no live code: `pair-expired`; 3. the code belongs to
  another Origin: `pair-busy`; 4. check the code: `token`, `bad-code` or (5th miss) `pair-locked`.

So a live 20th code still answers `pair-busy` to another Origin, and burning the 20th code answers
`pair-locked` until the lock ends, then `pair-disabled`.

### discover

The TVs adb sees over mDNS (`_adb-tls-connect`, `_adb-tls-pairing`, `_adb`), one entry per IP in a home
network range. `connectPort` / `pairPort` are `null` when not announced.

The bridge queries from ephemeral ports and never binds UDP 5353. Most TVs answer that query directly. Android 14
and later answer the pairing service only by multicast to 5353, so on macOS the bridge also asks the system's own
resolver (mDNSResponder, through dns_sd) for it. On Windows and Linux there is no such source yet: for those
Android 14+ TVs `pairPort` stays `null` there, and the site asks for the port the TV shows. TVs that answer the
query directly still report `pairPort` on every OS.

### pair

adb pairing with the TV (the 6 digits on the TV's Wireless debugging screen), not site pairing. `ip` must be a
home-network IPv4 (else `ip-not-allowed`), `port` an integer 1..65535 and `code` exactly 6 digits (else
`bad-request`). A wrong code is `pairing-failed`.

### connect

`ip` must be a home-network IPv4: dotted, no leading zeros, in 10/8, 172.16/12, 192.168/16 or 169.254/16; else
`ip-not-allowed` (checked first). `port` is optional: absent or `null` takes the `connectPort` that mDNS
announces for that IP, else 5555; any other non-port value is `bad-request`. A port is a JSON number whose
value is an integer 1..65535 (`5555`, `5555.0` and `5.555e3` are the same number; `"5555"` is not a port).

The result's `serial` is the adb serial (`ip:port`), and `model` the device's `ro.product.model`, trimmed, or
`""` when it cannot be read. How the bridge reads it (the TS bridge runs `getprop ro.product.model`; a bridge
may take it from the adb connection banner) is not part of the protocol. A socket may only use serials it connected itself; another
socket's serial is `unknown-serial`. Connecting the same serial again replaces (and closes) the old
connection. When a socket closes, every device it holds is closed.

### run

Runs `cmd` (a non-empty string of at most 256 Ki characters, else `bad-request`) in `adb shell` and returns its
`exitCode`, `stdout` and `stderr` unchanged. A non-zero exit is a normal result, not an error.

### writeFile

Writes the string `data` (UTF-8) to `path` on the TV. `path` must be a normalised absolute path strictly below
`/sdcard/ReTV/`: at most 512 characters, no `.` or `..` segments, no `//`, no trailing `/`, no NUL, CR or LF;
else `path-not-allowed`. `data` not a string is `bad-request`. This limit guards against path mistakes; it is
not a security boundary (`run` can write anywhere adb shell can).

### writeBytes

Uploads a binary file (the ReTV Home APK) in base64 chunks that each fit in a message, then pushes it to the TV
in one go after the last chunk.

- `path`: as for `writeFile`, or exactly `/data/local/tmp/retv-home.apk`; else `path-not-allowed`.
- `total`: the file size, an integer 1..32 MiB (33 554 432); `offset`: where this chunk starts; `data`: the
  chunk as strict padded standard base64, not empty. Anything else is `bad-request`.
- `offset: 0` starts a new upload, dropping any unfinished one. Each next chunk must continue it: same serial,
  path and total, `offset` equal to the bytes received so far, and not past `total`. A well-formed chunk that
  does not continue it drops the upload and is `bad-request`.
- A chunk rejected by its own checks (`unknown-serial`, `path-not-allowed`, or `bad-request` for bad base64,
  empty `data`, a non-number `offset`, a `total` that is not an integer 1..32 MiB) leaves the upload as it
  was: the site may resend that chunk.
- Base64 must be canonical: padded, standard alphabet, no whitespace, unused trailing bits zero (`YR==` is
  refused; `YQ==` is `a`).
- One upload per socket. The result is `{ received, done }`; the file reaches the TV only when `done` is true.

### disconnect

Closes the device and forgets the serial (and its upload, if any). The serial is then `unknown-serial`.

## Checks, in order

For requests that need a token: token (`bad-token`, socket closes), then the request's own checks in this
order: the serial (`unknown-serial`) for `run`, `writeFile`, `writeBytes`, `disconnect`; the IP
(`ip-not-allowed`) for `connect`, `pair`; the path (`path-not-allowed`); then the other parameters
(`bad-request`).

## Errors

| error | when | result |
| --- | --- | --- |
| `bad-request` | malformed message, unknown type, bad parameter | none |
| `bad-token` | missing or wrong token; the socket closes (1008) | none |
| `unknown-serial` | a serial this socket did not connect | none |
| `ip-not-allowed` | `connect` / `pair` outside the home network | none |
| `path-not-allowed` | `writeFile` / `writeBytes` outside the allowed paths | none |
| `bad-code` | wrong pairing code | `{ attemptsLeft }` |
| `pair-locked` | too many wrong codes | `{ retryIn }` (seconds) |
| `pair-busy` | another Origin's code is live | `{ retryIn }` (seconds) |
| `pair-expired` | no live code | none |
| `pair-disabled` | this run's 20 codes are used up | none |
| `too-many-requests` | 32 requests already running on this socket | none |
| `internal` | any failure without one of the codes here | none |

adb failures pass through as the engine's transport codes: `adb-server-unreachable`, `unauthorized`,
`device-offline`, `device-unreachable`, `device-not-found`, `pairing-failed`, `connection-lost`.
When a request finds the bridge's own adb server gone, the bridge starts it again and retries the request once
(at most one restart per 30 s). `adb-server-unreachable` therefore means the restart did not help: the site
shows its "adb is down" card, as for `hello` with `adb: false`.

`error` is always one of the codes on this page. Any other failure (adb or device error text, stderr, paths,
stack traces) is answered exactly `internal`; its text goes to the bridge's own log and never to the site.
Clients must treat an error code they do not know as `internal`.

## Limits

| Limit | Value |
| --- | --- |
| Message size | 1 MiB (1 048 576 bytes) |
| `run` command | 256 Ki characters |
| Device path | 512 characters |
| Upload (`writeBytes`) | 32 MiB, one per socket |
| Request `id` string | 64 characters |
| Pairing code | 6 digits, 120 s, single-use |
| Pairing misses before a lock | 5 |
| Pairing lock | 30 s, doubling, at most 1 h |
| Codes per run | 20 |
| Sockets open at once | 16 (then `503`) |
| Requests running per socket | 32 (then `too-many-requests`) |
| Request head | 16 KiB (then `431`) |

## Logging

The bridge logs every connection, refusal, command (`run` text clipped to 2000 characters), file write and
device connect/disconnect to its console. It never logs the session token or a pairing code (the code appears
only in the bridge window's frame).

## Protocol v2

Everything in v1 holds, with these additions. A v2 bridge answers `hello` with `protocol: 2`.

### hello: `features`

`hello`'s result gains `features`, the list of v2 requests this bridge serves: `["installApk"]`.

```json
{ "version": "...", "protocol": 2, "features": ["installApk"], "adb": true, "paired": true }
```

A v1 bridge sends no `features`; the site treats that as `[]`. The site accepts protocol 1 or 2 (anything else
is "update the bridge") and uses `installApk` only when `features` contains it, otherwise the v1 `writeBytes`
upload.

### installApk

`{ "id", "token", "type": "installApk", "serial", "url", "sha256" }` → `{ "output" }`

The bridge downloads the APK itself from the ReTV site and installs it, so the site does not upload it in chunks.

Checks, in order (after the token): `serial` (`unknown-serial`), `url` (`url-not-allowed`), `sha256`
(`bad-request` unless exactly 64 lowercase hex digits), then `busy` if this socket already has an install
running (one install per socket at a time; other sockets are not refused).

Installs to the same TV (serial) from different sockets take turns: their downloads may run side by side, but
steps 4–6 below run for one install at a time per serial, in arrival order, since they share the staging file.
When a socket closes, its install stops: a download is dropped; once step 4 began, the staged file is still
removed (step 6) before the socket's TVs are closed (the bridge waits at most 10 s for that; this wait is for an
install only, other requests are dropped at once). The closed socket gets no reply.

`url` is allowed (`isAllowedApkUrl`) when it is a string that:

- starts with `https://retv.app/downloads/` or `https://ru.retv.app/downloads/` (exact, case-sensitive: no other
  scheme, host, port, user info or letter case);
- continues with a path matching `^[A-Za-z0-9._/-]+\.apk$`;
- contains no `..`, `?` or `#` anywhere.

Then, each step only if the one before it succeeded:

1. **Download** with redirects refused (TS: `fetch(url, { redirect: "error" })`). Any 3xx status (300–399:
   a redirect with or without `Location`, `300`, `304`, ...), a network failure or a broken body is
   `download-failed` with no result; any other status than 200 is `download-failed` with `result: { status }`.
   A download takes at most 5 minutes.
2. **Size**: at most **64 MiB** (67 108 864 bytes), checked against `Content-Length` before reading and while
   reading (the bridge stops reading past the limit): else `too-large`. Exactly 64 MiB is accepted.
3. **SHA-256** of the bytes, as lowercase hex, equal to `sha256`: else `sha256-mismatch`.
4. **Stage** the bytes at `/data/local/tmp/retv-install.apk` (one push).
5. **Install**: run `pm install -r --user current /data/local/tmp/retv-install.apk`. Success when its stdout
   contains `Success` (the exit code is not used): the result is `{ output }`. Otherwise `install-failed` with
   `result: { output }`. `output` is pm's stdout followed by its stderr, trimmed, at most 1000 characters.
   `--user current` installs for the foreground user only. Without it pm installs for every Android user, and a
   Google TV kids profile is a separate user: the app would appear there without the parent's consent.
6. **Clean up**: once step 4 began, always run `rm -f /data/local/tmp/retv-install.apk` (after success, after
   `install-failed`, and after a staging or transport failure; its own failure is ignored). Steps 1–3 touch
   nothing on the TV, so their failures run nothing.

Failures inside steps 4–6 that are not the codes above follow the usual rule: a transport code passes through,
a full TV is `no-space`, anything else `internal`.

`pm install` output is device text that does reach the site (it says why Android refused the APK, e.g.
`Failure [INSTALL_FAILED_VERSION_DOWNGRADE]`); it holds no paths of the computer running the bridge.

### no-space

A failure whose text contains `no space left on device` (any letter case), from any request (`writeFile`,
`writeBytes`, `run`, `installApk`), is answered `no-space` instead of `internal`, so the site can say the TV's
storage is full. Its text still stays in the bridge's own log.

### v2 errors

| error | when | result |
| --- | --- | --- |
| `url-not-allowed` | `installApk` URL fails `isAllowedApkUrl` | none |
| `download-failed` | any 3xx, network failure, broken body, or another HTTP status than 200 | `{ status }` for a non-3xx HTTP status, else none |
| `too-large` | the APK is over 64 MiB | none |
| `sha256-mismatch` | the bytes do not match `sha256` | none |
| `install-failed` | `pm install` stdout has no `Success` | `{ output }` |
| `busy` | this socket already has an `installApk` running | none |
| `no-space` | the TV reports no space left on device | none |

### v2 limits

| Limit | Value |
| --- | --- |
| `installApk` download | 64 MiB, 5 minutes, one per socket |
| `install-failed` / result `output` | 1000 characters |
