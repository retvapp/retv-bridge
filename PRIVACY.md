# Privacy

ReTV Bridge has no accounts, no analytics and no telemetry. It sends no identifiers. It makes network connections only to:

| Destination | Why | When |
|---|---|---|
| `dl.google.com` | Download Google's adb platform-tools | Once, if adb is not already present |
| `retv.app` and `ru.retv.app`, path `/downloads/retv-bridge.json` | Check whether a newer Bridge exists | When the Bridge starts, then once in 24 hours (a failed check is retried after 15 minutes, then 30 minutes, 1 hour … at most daily), and when you click «Check for updates». The site of your system language is asked first. The request carries only `User-Agent: ReTV-Bridge/<version>` |
| `retv.app` or `ru.retv.app`, path `/downloads/ReTV-Bridge-*` | Download the update | Only when you click «New version … available» |
| `retv.app` and `ru.retv.app`, path `/downloads/*.apk` | Fetch an APK to install on your TV (`installApk`) | Only when the ReTV site asks you to install something |
| TVs on your local network | adb over TCP to the devices you choose | When you use ReTV |

Like any web server, `retv.app` and `ru.retv.app` see the IP address of every update check, update download and APK download, and record it with the time, path and `User-Agent` in their standard web-server logs. The Bridge adds no identifier of its own.

The Bridge listens only on `127.0.0.1`. It keeps a session token in memory and the adb platform-tools in your user profile. Nothing else is stored or uploaded.
