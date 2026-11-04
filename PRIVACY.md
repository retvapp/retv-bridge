# Privacy

ReTV Bridge needs no account (signing in to ReTV is optional) and has no analytics and no telemetry. Signed out, it sends no identifiers. It makes network connections only to:

| Destination | Why | When |
|---|---|---|
| `dl.google.com` | Download Google's adb platform-tools | Once, if adb is not already present |
| `retv.app` and `ru.retv.app`, path `/downloads/retv-bridge.json` | Check whether a newer Bridge exists | When the Bridge starts, then once in 24 hours (a failed check is retried after 15 minutes, then 30 minutes, 1 hour … at most daily), and when you click «Check for updates». The site of your system language is asked first. The request carries only `User-Agent: ReTV-Bridge/<version>` |
| `retv.app` or `ru.retv.app`, path `/downloads/ReTV-Bridge-*` | Download the update | Only when you click «New version … available» |
| `retv.app` and `ru.retv.app`, path `/downloads/*.apk` | Fetch an APK to install on your TV (`installApk`) | Only when the ReTV site asks you to install something |
| TVs on your local network | adb over TCP to the devices you choose | When you use ReTV |
| Devices on your local network that answer DIAL (Fire TV, Chromecast, smart TVs) | An SSDP search for DIAL (multicast `239.255.255.250:1900`), then a read-only HTTP GET of each answering device's own description, and of its `FireTVRemote` DIAL app entry if the description does not say Amazon, to tell a Fire TV apart | When the site searches for TVs |
| Samsung TVs on your local network | SSDP search (multicast `239.255.255.250:1900`), the TV's REST API (port 8001), sdb (port 26101, Developer Mode) and the TV's remote-control channel (port 8002) | Only in the «Samsung» tab, when you press its buttons |
| `account.samsung.com` | Samsung's sign-in page, in your browser; Samsung sends the result to `http://localhost:4794/signin/callback` on this computer | Only when you press «Sign in to Samsung» |
| `svdca.samsungqbe.com` | Samsung's certificate service: two certificate requests (public keys only) with the sign-in token, your Samsung user id and the TV's device id (DUID) | Right after that sign-in |
| `retv.app` or `ru.retv.app` (the one picked under «Account on»), paths `/v1/apps/*` | «Sign in to ReTV»: asks for a sign-in code with this computer's name, its OS line (e.g. «macOS 15») and the Bridge version, which the site shows when you allow it; the sign-in page opens in your browser. Then the account's label, plan and TVs (with the ad setting a Samsung TV's ReTV Home reported, read while «Tracking and ads» runs), and while it runs a short request for that TV («check the ad setting», with a random check id) that ReTV Home on the TV takes from the account, a refresh of the sign-in about once an hour, and «Sign out» | Only after you click «Sign in to ReTV», and while you stay signed in |

Like any web server, `retv.app` and `ru.retv.app` see the IP address of every update check, update download and APK download, and record it with the time, path and `User-Agent` in their standard web-server logs. The Bridge adds no identifier of its own.

The Bridge listens only on `127.0.0.1` (and, during a Samsung sign-in, on `127.0.0.1` and `::1` port 4794 until the sign-in arrives). It keeps a session token in memory and the adb platform-tools in your user profile. For Samsung TVs it also keeps, in its own data folder (`samsung/`, readable only by you): each TV's certificate with its private keys, the remote-control pairing the TV issued, and the «как было» app lists saved before removals. The Samsung sign-in token is kept in memory only and is never written or logged. Signed in to ReTV, the refresh token (with the site, the sign-in id, and the account label and plan the header shows) is kept in the macOS Keychain or the Windows Credential Manager, never in a file or the log; the short-lived access token stays in memory. «Sign out» removes it. Nothing else is stored or uploaded.
