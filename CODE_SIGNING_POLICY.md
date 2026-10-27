# Code signing policy

Free code signing provided by [SignPath.io](https://signpath.io), certificate by [SignPath Foundation](https://signpath.org).

## What is signed

Only release builds of ReTV Bridge, made by GitHub Actions on GitHub-hosted runners from tags matching `bridge-v*`, are submitted for signing. Nothing built on a developer's machine is signed with this certificate.

## Team roles

- **Committers and reviewers:** the project maintainers who have write access to this repository. Changes reach the release branch through review.
- **Approver:** the project owner. Each signing request is approved manually by the owner; no request is auto-approved.

Roles are named by role, not by person. All team members use multi-factor authentication on GitHub and SignPath.

## Privacy

This program will not transfer any information to other networked systems unless specifically requested by the user or the person installing or operating it. See [PRIVACY.md](PRIVACY.md) for the complete list of connections.
