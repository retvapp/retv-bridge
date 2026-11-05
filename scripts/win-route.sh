#!/usr/bin/env bash
# Prints the Windows route of a release (UNSIGNED or SIGNPATH) from the committed release-win-route file, and fails when
# the repository variable SIGNPATH_ORGANIZATION_ID disagrees with it. The file decides, as release-team-id does for the
# Mac, so a variable alone can neither drop the signature nor ask for one. scripts/release-retv-bridge.sh in the monorepo
# checks the release against the same file.
#
# Usage: scripts/win-route.sh [file]   (default: release-win-route at the bridge root)
# Env: SIGNPATH_ORGANIZATION_ID.
set -euo pipefail

FILE="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/release-win-route}"
die() { echo "win-route: $*" >&2; exit 1; }

[ -f "$FILE" ] || die "$FILE is missing: write UNSIGNED or SIGNPATH there"
route="$({ grep -v '^[[:space:]]*#' "$FILE" || true; } | tr -d '[:space:]')"
org="${SIGNPATH_ORGANIZATION_ID:-}"
case "$route" in
  UNSIGNED) [ -z "$org" ] || die "release-win-route says UNSIGNED, but SIGNPATH_ORGANIZATION_ID is set: delete the variable, or write SIGNPATH there to sign" ;;
  SIGNPATH) [ -n "$org" ] || die "release-win-route says SIGNPATH, but SIGNPATH_ORGANIZATION_ID is not set: set the variable, or write UNSIGNED there" ;;
  *) die "release-win-route says '$route': it must say UNSIGNED or SIGNPATH" ;;
esac
printf '%s\n' "$route"
