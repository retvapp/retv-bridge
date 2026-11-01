# ReTV Bridge protocol

The ReTV site (the ReTV engine running in the browser) drives a real TV through the bridge: a small server on
the person's computer that runs the site's commands on the TV with adb. This document is the wire contract
between the site and any bridge (the TypeScript reference bridge in the ReTV monorepo, the Rust bridge in
this repository). The golden fixtures in [`conformance/`](conformance/README.md) pin it down; both bridges
must pass them.

- **Protocol v1**: below. The TypeScript bridge 0.1.x defines it.
- **Protocol v2**: v1 plus `hello`'s `features`, the `installApk` request and the `no-space` error code; see
  [Protocol v2](#protocol-v2).
- **Protocol v3**: v2 plus the `allowlist` feature: `run` takes only the command shapes the ReTV engine sends,
  anything else is `command-not-allowed`; see [Protocol v3](#protocol-v3). Both bridges speak v3. The site
  accepts a bridge on protocol 1, 2 or 3.
- **Helper profile**: ReTV Helper, an app on the TV, serves protocol v3 to the site through the ReTV relay, with the
  identical `run` allow-list; see [Helper profile](#helper-profile) at the end.

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
`exitCode`, `stdout` and `stderr` unchanged. A non-zero exit is a normal result, not an error. In protocol v3 `cmd` must
be on the [allow-list](#protocol-v3) (else `command-not-allowed`), and the bridge runs the line it rebuilds from it.

### Android users

A Google TV profile is an Android user, and per-user work goes through `run`. The bridge never adds, removes or
rewrites `--user` in `cmd`: the line the site sends is the line that runs (after the v3 allow-list rebuild, which keeps
`--user N` as given). Choosing the user is the site's job. `installApk` always uses `--user current` and takes no user
parameter. To give a profile an app, the site runs `cmd package install-existing --user N <package>` through `run`.
A refusal such as `java.lang.SecurityException: Shell does not have permission to access user N` is pm's answer
(`ok: true`, non-zero `exitCode`, text in `stderr`), not a bridge error. The protocol number does not change.
Fixture: `conformance/wire/run-per-user.json`.

### writeFile

Writes the string `data` (UTF-8) to `path` on the TV. `path` must be a normalised absolute path strictly below
`/sdcard/ReTV/`: at most 512 characters, no `.` or `..` segments, no `//`, no trailing `/`, no NUL, CR or LF;
else `path-not-allowed`. `data` not a string is `bad-request`. In protocol v1 and v2 this limit guards only against
path mistakes (`run` can write anywhere adb shell can); in v3 `run` cannot write files outside `/sdcard/ReTV/`, so it is
part of the boundary.

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
| `pairing-needed` | helper profile: wireless `adbStart` before our key is paired with the TV | none |
| `wireless-off` | helper profile: no `_adb-tls-connect` service within 5 s (Wireless debugging is off) | none |
| `no-pairing-service` | helper profile: `adbPair` finds no `_adb-tls-pairing` service within 5 s | none |
| `adb-not-ready` | helper profile: `connect` before the session has adb (`adbStart` / `adbPair`) | none |

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

The bridge logs every connection, refusal, command (`run` text clipped to 2000 characters; in v3 the rebuilt line,
or the refused text), file write and device connect/disconnect to its console. It never logs the session token or a pairing code (the code appears
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

## Protocol v3

Everything in v2 holds, with these changes. A v3 bridge answers `hello` with `protocol: 3` and
`features: ["installApk", "allowlist", "siteSignIn"]`. `siteSignIn`: the allow-list takes ReTV Home's site sign-in broadcast (`--es code` included) and never logs the code; the site sends the code only to a bridge or Helper that announces it.

```json
{ "version": "...", "protocol": 3, "features": ["installApk", "allowlist", "siteSignIn"], "adb": true, "paired": true }
```

### Why

In v1 and v2, `run` takes any shell command, so whoever holds the session token has a full adb shell on every paired
TV: an XSS on the site or a compromised deploy becomes a remote shell. In v3 `run` takes only the command shapes the
ReTV engine sends, each argument checked, and nothing else. A token holder can still do what the site does (turn apps off
and on, change the settings below), but it cannot install an APK, read files outside `/sdcard/ReTV/` and the installed
apps' icons, or run anything else.

### run in v3

Checks, in order (after the token): `serial` (`unknown-serial`), `cmd` a non-empty string of at most 256 Ki characters
(`bad-request`), then the allow-list: `cmd` that is not one of the shapes below is `command-not-allowed`. Nothing reaches
the TV and the socket stays open. Otherwise the bridge runs the line it rebuilds from the checked words, never the text
it received.

**Words.** `cmd` is split with a strict subset of sh syntax; anything outside it is refused:

- printable ASCII only (0x20–0x7E): no tab, CR, LF, NUL or non-ASCII;
- words are separated by exactly one space; no leading, trailing or double space;
- a word joins unquoted runs of `A–Z a–z 0–9 _ . , : / = + @ ? -`, single-quoted strings (anything printable but `'`)
  and `\'` (an escaped quote, as the engine's `shellQuote` writes it). `''` is the empty word;
- a lone unquoted `|` word separates pipeline stages (only the fixed filters below use one);
- everything else is refused: `;`, `&`, `>`, `<`, `` ` ``, `"`, and outside quotes `$`, `*`, `~`, `(`, `)`, `{`, `}`,
  `[`, `]`, `#`, `!`; a `\` that does not escape `'`; an unclosed quote.

**The rebuilt line.** Each word goes out bare when it is not empty and consists only of
`A–Z a–z 0–9 _ . , : / = + @ -`, else single-quoted (a `'` inside written as `'\''`); words are joined by one space and
stages by ` | `. So `settings put global animator_duration_scale '1.0'` runs as
`settings put global animator_duration_scale 1.0`, and `am start ... -d market://details?id=ru.kinopoisk.tv` runs with
`'market://details?id=ru.kinopoisk.tv'` quoted.

**Typed words.**

| Type | Rule |
| --- | --- |
| `<pkg>` | a package name: `^[A-Za-z_][A-Za-z0-9_]*(\.[A-Za-z0-9_]+)+$`, at most 255 characters |
| `<component>` | `<pkg>/<class>`, the class `[A-Za-z0-9_.$]+` (`.Main`, `com.x.Main`, `.Outer$Inner`), at most 512 in all |
| `<prop>` | `^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$` |
| `<seconds>` | `1`..`3600`, no leading zero |
| `<hex64>` | exactly 64 lowercase hex digits |
| `<layout-backup>` | `/sdcard/Download/ReTV/retv-home-layout.json` or `/sdcard/Download/ReTV/retv-home-layout (1).json` … `(9).json` (quoted, it has a space) |
| `<backup-b64>` | standard base64 (`[A-Za-z0-9+/]`, then at most two `=`), a multiple of 4, 4..96000 characters |
| `<global-key>` | `window_animation_scale`, `transition_animation_scale`, `animator_duration_scale`, `private_dns_mode`, `private_dns_specifier`, `always_finish_activities`, `adb_enabled` |
| `<secure-key>` | `enabled_accessibility_services`, `accessibility_enabled`, `screensaver_components`, `limit_ad_tracking` |
| `<scale>` | a decimal 0..10: `^(10(\.0+)?\|[0-9](\.[0-9]+)?)$` (`0`, `0.5`, `1.0`, `10`) |
| `<host>` | a Private DNS host name: `^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9-]+)+$`, at most 253 characters (lowercase, at least one dot, no port) |
| `<a11y-list>` | `<component>` joined by `:`, or empty; at most 4096 characters |
| `<screensaver-list>` | `<component>` joined by `,`, or empty; at most 4096 characters |
| `<retv-file>` | `/sdcard/ReTV/<name>`, the name `[A-Za-z0-9._-]{1,128}` and not `.` or `..` (one level, no subfolders) |
| `<apk>` | below `/data/app/`, `/system/`, `/system_ext/`, `/product/` or `/vendor/`, ending in `.apk`, at most 512 characters, each segment `[A-Za-z0-9._~=+-]+` and not `.` or `..` |
| `<icon>` | `res/<dir>/<file>.png` or `.webp`, `<dir>` and `<file>` of `[A-Za-z0-9_.-]`, `<dir>` not `.` or `..` |
| `<market-url>` | `market://details?id=<pkg>` |
| `<op>` | `RECORD_AUDIO`, `RUN_ANY_IN_BACKGROUND` |
| `<mode>` | `allow`, `ignore`, `deny`, `default`, `foreground` |

The settings keys are the engine's `MANAGED_SETTINGS` (what a snapshot keeps and restore writes back) plus `adb_enabled`
(«finish» turns debugging off). Below, `a | b` in a cell means one of the literal words; everything else is literal.

**Reads.**

| Command |
| --- |
| `getprop` · `getprop <prop>` |
| `cat /proc/meminfo` |
| `dumpsys` with one of `connectivity`, `diskstats`, `meminfo`, `package`, `usagestats`, `wifi` |
| `dumpsys meminfo <pkg>` · `dumpsys package <pkg>` · `dumpsys notification` (redacted: no `--noredact`) |
| `dumpsys package [<pkg>]`, piped to exactly the engine's microphone filter (`mic-audit.ts` `MIC_FILTER`, below) |
| `dumpsys package <pkg>`, piped to `grep -E '^ *(Package \[\|versionCode=\|versionName=)'` |
| `pm list packages` · `pm list packages` with one of `-3`, `-d`, `-e`, `-s`, `-u` · `pm list packages -d -u` · `pm list packages <pkg>` · `pm list packages -e <pkg>` |
| `pm path <pkg>` · `pm path --user current <pkg>` |
| `pm query-services --components -a` with one of `android.net.VpnService`, `android.accessibilityservice.AccessibilityService`, `android.media.tv.TvInputService`, `android.speech.RecognitionService`, `android.service.voice.VoiceInteractionService`, `android.service.dreams.DreamService` |
| `cmd package query-activities --brief -a android.intent.action.VOICE_COMMAND` |
| `cmd package query-activities` or `resolve-activity`, then `--brief -a android.intent.action.MAIN -c android.intent.category.HOME` |
| `content query --uri content://android.media.tv/channel --projection package_name:display_name:browsable:type` |
| `ime list -a -s` |
| `cmd appops get [--user 0] <pkg> <op>` · `cmd appops query-op [--user 0] <op> <mode>` |
| `cmd appops get app.retv.home REQUEST_INSTALL_PACKAGES` (the snapshot records ReTV Home's install permission; that package only) |
| `settings get global <global-key>` · `settings get secure <secure-key>` · `settings list` with one of `global`, `secure`, `system` |
| `unzip -l <apk> 'res/*'`, piped to `grep -E` with exactly the engine's icon filter (`installed.ts` `ICON_GREP`), piped to `head -n 64` |
| `unzip -p <apk> <icon>`, piped to `base64` |
| `ls /sdcard/ReTV/` · `cat <retv-file>` |
| `ls /sdcard/Download/ReTV/` · `head -c 72001 <layout-backup>` (ReTV Home's own layout backups, read with a bound) |

**Changes.**

| Command |
| --- |
| `mv <retv-file> <retv-file>` · `rm -f <retv-file>` · `rm -f /data/local/tmp/retv-home.apk` (the v1 upload's staging file) |
| `pm disable-user --user 0 <pkg>` · `pm enable --user 0 <pkg>` |
| `pm` with one of `disable`, `disable-user`, `default-state`, `enable`, then `--user 0 <component>` |
| `cmd package install-existing --user 0 <pkg>` · `pm uninstall -k --user 0 <pkg>` |
| `pm grant --user 0 <pkg> android.permission.RECORD_AUDIO` · the same with `revoke` |
| `pm grant --user 0 app.retv.home android.permission.READ_LOGS` · the same with `revoke` (ReTV Home only) |
| `pm grant --user 0 app.retv.home android.permission.READ_TV_LISTINGS` · the same with `revoke` (ReTV Home only) |
| `cmd appops set [--user 0] <pkg> <op> <mode>` |
| `cmd appops set app.retv.home REQUEST_INSTALL_PACKAGES allow\|default\|ignore\|deny` (ReTV Home installs its own updates, and restore puts back the recorded mode; that package only) |
| `cmd package set-home-activity <component>` · `cmd package set-home-activity --user 0 <component>` |
| `settings put global window_animation_scale\|transition_animation_scale\|animator_duration_scale <scale>` |
| `settings put global private_dns_mode off\|opportunistic\|hostname` · `settings put global private_dns_specifier <host>` |
| `settings put global always_finish_activities\|adb_enabled 0\|1` · `settings put secure accessibility_enabled 0\|1` |
| `settings put secure enabled_accessibility_services <a11y-list>` · `settings put secure screensaver_components <screensaver-list>` |
| `settings put secure limit_ad_tracking 0\|1` |
| `settings delete global <global-key>` · `settings delete secure <secure-key>` |
| `pm trim-caches 999G` |
| `timeout <seconds> cmd package bg-dexopt-job` · `timeout <seconds> cmd package compile -m speed-profile -a` |
| `am broadcast -n app.retv.home/.data.DeviceKeyReceiver -a app.retv.home.action.DEVICE_KEY -f 32 --es key <hex64>` |
| `timeout 10 am broadcast --user <user> -n app.retv.home/.data.DisabledSetReceiver -a app.retv.home.action.DISABLED_SET -f 32 --ei seq <n> --ei total <n> --es pkgs <pkg-list>` (the packages ReTV turned off, for that user's ReTV Home; `<user>` `0` or another user's id; `<n>` 1..99, `<pkg-list>` packages joined by `,`, at most 1000 characters, or `-` for none) |
| `am broadcast -n app.retv.home/.data.LayoutBackupReceiver -a app.retv.home.action.LAYOUT_BACKUP -f 32 --es backup <backup-b64>` |
| `am broadcast -n app.retv.home/.a11y.HomeOverrideReceiver -a app.retv.home.action.HOME_OVERRIDE -f 32 --ez consent true` (or `false`) |
| `am start -a android.intent.action.VIEW -d <market-url>` |
| `am start -a` with one of `com.google.android.gms.settings.ADS_PRIVACY`, `android.settings.PRIVACY_SETTINGS`, `android.settings.SETTINGS` (open a settings screen; no component, no extras) |

The fixed filters, verbatim as the engine sends them (`[--user 0]` above means with or without exactly `--user 0`):

```
grep -e 'Package \[' -e 'SharedUser \[' -e userId= -e sharedUser= -e pkgFlags= -e versionCode= -e 'User ' -e RECORD_AUDIO -e '^[A-Z]'
grep -E 'res/(mipmap|drawable)[^/]*/(ic_launcher|app_icon|icon|ic_app|launcher_icon)(_round)?[.](png|webp)$'
```

Not on the list, on purpose: `pm install` in any form (an APK reaches the TV only through `installApk`, which downloads
from the ReTV site and checks the SHA-256 the site pins), `pm clear`, `input`, `reboot`, `sh`, `su`, `logcat`, `echo`,
any write outside `/sdcard/ReTV/`, settings keys other than the ones above, `am start` of anything but a Play Store page or the three settings screens above,
broadcasts to anything but ReTV Home's four receivers, and any other `dumpsys` service.

What a token holder can still do in v3, honestly: everything the site itself does, within the checks above.

- **Read** what the site reads: the package lists, `dumpsys` of notifications (redacted: packages and categories, never their text), usage stats,
  package details, Wi-Fi and connectivity state, memory and disk use, all of `getprop`, and `settings list` of all three
  namespaces (device names and other identifiers included), the snapshots in `/sdcard/ReTV/`, ReTV Home's layout
  backup in `Download/ReTV/` and installed apps' icons.
- **Change** what the site changes: turn any app or component off or on, remove an app for the user (it stays
  restorable with `install-existing`), grant or revoke the microphone, set any app op of the two above, set the home
  screen, trim caches and run the compile jobs, and the settings above. On a fresh ReTV Home install (no layout yet) it
  can hand ReTV Home a layout backup; the app only offers it («Восстановить раскладку?»), the person decides. A
  handed-over copy is a file ReTV Home may not have written: it restores the layout parts only, and its start target
  and remote buttons only with «Восстановить и это».
- **Two changes outlast the session.** Private DNS can be pointed at any host name (`private_dns_mode hostname` plus
  `private_dns_specifier <host>`): the TV then resolves every name through that server until someone changes it back.
  And `enabled_accessibility_services` can switch on the accessibility service of any app already installed, which can
  read the screen and send keys.

It cannot install an APK the ReTV site does not serve, run any other command, write outside `/sdcard/ReTV/`, or read
other files. The mitigations around what remains:

- the session token is local: it changes on every bridge start and only reaches this computer's browser (the link's
  fragment, or the 6-digit pairing in the bridge window);
- the bridge listens on loopback only and accepts only the ReTV sites' `Origin`;
- the site's CSP runs only scripts the site ships as files from its own origin (no inline script, no `eval`), so an
  injected script needs a compromised deploy, not a markup bug;
- a snapshot is taken before every change, and «Вернуть как было» (restore) writes the Private DNS settings and the
  accessibility services back as they were.

`conformance/commands.json` pins the list: each vector is a command and the rebuilt line, or `null` for a refusal. Both
bridges keep the table in the same order (the TS bridge's `commands.ts`, Rust `crates/core/src/commands.rs`).

### Development builds: installApk from a dev site

A release bridge downloads only from the ReTV site (`isAllowedApkUrl`). A development bridge also takes a dev site's
APK, so local walkthroughs that serve ReTV Home from the site (`scripts/dev-stack.sh` in the ReTV monorepo) keep working:
`http://` + `localhost`, `127.0.0.1` or a home-network IPv4 (as for `connect`), `:<port>` (plain decimal 1..65535), then
`/downloads/` and the same path rule (`[A-Za-z0-9._/-]+\.apk`, no `..`, `?` or `#`). Everything else about `installApk`
(the SHA-256 pin, size, no redirects) is unchanged.

- Rust bridge: debug builds only (`#[cfg(debug_assertions)]`, `validate::DEV_APK_URLS`); a release build never takes
  such a URL. `cargo test --release -p retv-bridge-core` runs the release side of that test.
- TS bridge: only when started with `--dev`; off by default.

The site offers such a URL only from a non-production build and only to a bridge with `allowlist`.

### The site with a v3 bridge

- The site sends `hello` with `version: 3` and accepts protocol 1, 2 or 3; an older bridge keeps working as before.
- With `allowlist` the site installs every APK (ReTV Home and the alternatives) with `installApk` and never uploads one
  for `pm install`. When `installApk` cannot be used (a page not served over https, a host the bridge does not download
  from), the install fails instead.
- A `command-not-allowed` reply is the site's own bug, not a TV that stopped answering: the engine sees a failed command
  (`exitCode` 126, `stderr` `command-not-allowed`), that step fails, and the console names the command. While a
  snapshot is taken, a refused read fails the snapshot (and so the run, before any change) instead of passing for
  «nothing there».

### v3 errors

| error | when | result |
| --- | --- | --- |
| `command-not-allowed` | `run` with a command off the allow-list | none |

## Helper profile

ReTV Helper is an app on the TV. It serves this protocol to the site in the phone's browser, through the ReTV relay,
and runs the commands on the TV it lives on through that TV's own adb (classic `127.0.0.1:5555`, or Wireless debugging).
It speaks **protocol v3 with a profile of its own**: everything above holds, except as below.

### Transport and session

- Each message is one JSON object as UTF-8 text, sealed in one frame of the end-to-end encrypted session between the
  phone and the TV (`conformance/README.md`, "e2e/*.json"). The relay sees ciphertext only.
- The same 1 MiB message limit applies to the text: a bigger one closes the session; one of exactly 1 MiB is still read.
- **Replies over 1 MiB go in parts.** The relay carries no frame over 1 MiB + 1 KiB (it closes the sender, 1009), and a
  `run` reply can be bigger (`dumpsys package` is 1.9 MB on a Box S3). So the Helper sends a reply whose UTF-8 text is
  over 1 MiB as its UTF-8 bytes in order, at most 512 KiB per part, each part sealed in its own frame as
  `{ "id": <the reply's id>, "part": <0, 1, …>, "parts": <count>, "data": "<the bytes in standard base64>" }`, one
  right after the other. The site joins the parts of that `id` and reads the result as the reply. A part out of order,
  a count that changes, bad base64 or a join that is not that reply fails that request (`bad-reply` on the site); the
  session goes on. A reply of at most 1 MiB is sent whole, as before.
- There is no WebSocket upgrade of ours, so no Origin / Host checks and no `426` / `403` / `503`. The E2E handshake (the
  QR secret, or the 6-digit SAS plus «Разрешить» on the TV) replaces the session token and site pairing.
- **No `token`.** A request needs none, and a `token` present (any value) is ignored: nothing is ever `bad-token`.
- The frame order, the 32 requests running at once (`too-many-requests`), the in-order state changes and the "Checks, in
  order" stay as they are, minus the token check.

### Requests

| type | params | result | errors |
| --- | --- | --- | --- |
| `hello` | none (`version` optional) | `{ version, protocol: 3, features: ["installApk", "allowlist", "helper", "siteSignIn"], adb, paired: true }` | none |
| `helperInfo` | none | `{ sdk, release, model, classicPort, adb: "ready" \| "none", method: "classic" \| "wireless" \| null }` | none |
| `adbStart` | `method`: `"classic"` \| `"wireless"` | `{ method }` | `unauthorized`, `device-unreachable`, `pairing-needed`, `wireless-off`, `bad-request` |
| `adbPair` | `code`: a string of exactly 6 digits | `{ method: "wireless" }` | `no-pairing-service`, `pairing-failed`, `wireless-off`, `bad-request` |
| `discover` | none | `{ tvs: [{ name: <model>, ip: "127.0.0.1", connectPort: null, pairPort: null }] }` | none |
| `connect` | `ip`: `"127.0.0.1"` | `{ serial: "self", model }` | `ip-not-allowed`, `adb-not-ready` |
| `run`, `writeFile`, `writeBytes`, `installApk`, `disconnect` | as in v3, with `serial: "self"` | as in v3 | as in v3 |
| `endSession` | `debugging`: `"off"` \| `"keep"` | `{ debugging: "off" \| "kept" }`, then the session closes | `bad-request` |

`pair`, `pairStart` and `pairFinish` do not exist in this profile: like any unknown type they are `bad-request`, and the
session stays open.

- **hello**: `protocol` is 3. `features` adds `helper`, which tells the site it talks to the Helper: the requests of this
  table exist and site pairing does not. `adb` is `true` once `adbStart` or `adbPair` has given the session an adb
  connection to the TV. `paired` is always `true`.
- **helperInfo**: `sdk` and `release` are the TV's Android API level and version, `model` its `ro.product.model`
  (`""` when unknown). `classicPort` tells whether `127.0.0.1:5555` answered. `adb` and `method` describe the session's
  adb connection (`"none"` and `null` before one).
- **adbStart**: `method` other than those two strings is `bad-request`. `classic` connects to `127.0.0.1:5555`: nothing
  answering there is `device-unreachable`; the TV then asks the person to allow our RSA key, and a refusal or no answer
  in 60 s is `unauthorized`. `wireless` needs our key paired with the TV first (`pairing-needed`, then `adbPair`), and
  Wireless debugging on: no `_adb-tls-connect` service within 5 s is `wireless-off`.
- **adbPair**: the 6 digits that the TV's Wireless debugging dialog for pairing with a code shows, typed on the phone. `code`
  that is not a string of exactly 6 digits is `bad-request` (checked first). No `_adb-tls-pairing` service within 5 s
  is `no-pairing-service`, a wrong code `pairing-failed` (the person may try again). On success the Helper pairs our
  key, then connects as `adbStart wireless` does. Below Android 12 (`sdk` < 31) it is always `no-pairing-service`: the
  pairing's TLS exporter is not public there, so the site offers the wireless route only from `sdk` 31.
- **discover** lists the TV itself only.
- **connect**: `ip` must be exactly `"127.0.0.1"`, else `ip-not-allowed` (checked first; any other address, `localhost`,
  or none). `port` is not used. Before `adbStart` / `adbPair` succeeded it is `adb-not-ready`. The serial is always
  `"self"`; any other serial, or `"self"` before `connect` or after `disconnect`, is `unknown-serial`.
- **run** takes the identical allow-list (see [Protocol v3](#protocol-v3)): the same table, in the same order, as the TS
  bridge's `commands.ts` and Rust `commands.rs`, pinned by the same `conformance/commands.json`, and it runs the same
  rebuilt line. Off the list is `command-not-allowed`.
- **writeFile**, **writeBytes**, **installApk** (`isAllowedApkUrl` only, no redirects, 64 MiB, the pinned SHA-256, the
  same staging file and `pm install` line) and **disconnect** are as in v3. `no-space` and the transport codes pass
  through as on the bridge.
- **endSession**: `debugging` other than those two strings is `bad-request` and the session stays. `"keep"` leaves adb
  as it is and answers `{ debugging: "kept" }`. `"off"` turns debugging off and answers `{ debugging: "off" }`: after a
  wireless session it runs `settings put global adb_wifi_enabled 0`; after a classic session it runs
  `settings put global adb_enabled 0`, as the last command, because that ends adbd and our connection with it. Either
  way the Helper then closes the session. When the session never had an adb connection to turn debugging off with,
  `"off"` answers `{ debugging: "kept" }`. These commands are the Helper's own, not `run`: the site cannot send
  `adb_wifi_enabled` (it is not on the allow-list).
- **The TV ends the session itself** («Отключить телефон» on the TV, or 30 min without a command): before it closes the
  socket, the Helper turns debugging off and seals one frame with no `id` to the phone:
  `{ "event": "session-ended", "by": "tv", "reason": "button" | "idle" | "other", "debugging": "off" | "kept" }`.
  `reason`: `button` is «Отключить телефон» on the TV, `idle` the 30 min without a command, `other` any later TV-side
  end; a site that does not know a reason treats it as `other`. The site then tells the person what ended the session,
  whether debugging is off on the TV and how to connect again; an older site ignores a frame without a known `id`. A
  session ended for a frame that did not open (a replay or tamper) sends nothing.

### Helper profile conformance

The wire fixtures whose `profiles` include `"helper"` pin this profile (`conformance/README.md`, "Profiles"): the
`helper-*.json` files, and the `run` allow-list, `writeFile`, `writeBytes`, `installApk` and `no-space` fixtures that
the bridges run too. The Helper runs them in process, along with `validators.json` and `commands.json`.
