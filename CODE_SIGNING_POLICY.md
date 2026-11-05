# Code signing policy

## What is signed

Release builds of ReTV Bridge are made by GitHub Actions on GitHub-hosted runners from tags matching `bridge-v*`.

- **Windows:** the installer has no code signature. Windows may warn that the publisher is unknown: press More info, then Run anyway.
- **macOS:** the app is ad-hoc signed and not notarized by Apple. macOS asks once before the first start.
- **Updates, on both systems:** every update is signed with the project's updater key, and the installed app refuses an update that does not verify against the public key it was built with.

The site lists the SHA-256 of every installer at https://retv.app/downloads.

## Team roles

- **Committers and reviewers:** the project maintainers who have write access to this repository. Changes reach the release branch through review.
- **Approver:** the project owner. Each release build is approved manually by the owner before it can use the updater key.

Roles are named by role, not by person. All team members use multi-factor authentication on GitHub.

## Privacy

This program will not transfer any information to other networked systems unless specifically requested by the user or the person installing or operating it. See [PRIVACY.md](PRIVACY.md) for the complete list of connections.
