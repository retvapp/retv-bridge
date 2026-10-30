# Security policy

Please report vulnerabilities privately to **security@retv.app**. Do not open a public issue for a security problem.

Include the version, your OS, and steps to reproduce. We aim to acknowledge a report within 7 days and to ship a fix for confirmed issues as a new signed release.

Supported: the latest released version.

Read the trust model in [README.md](README.md) first: a site that holds the session token can run the command shapes on the allow-list in [PROTOCOL.md](PROTOCOL.md#protocol-v3), and nothing else. That still lets a token holder read what the site reads (notifications, usage, `settings list`), point Private DNS at another server until it is changed back, and switch on an installed app's accessibility service; PROTOCOL.md lists all of it and the mitigations. Reports that restate that documented design are not vulnerabilities; reports of ways to get a token, to bypass the Origin or token checks, or to run anything the allow-list does not list (a command, an argument or a quoting trick that gets through), are.
