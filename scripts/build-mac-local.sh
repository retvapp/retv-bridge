#!/usr/bin/env bash
# Local fallback for the macOS release build (same steps as the `mac` job of
# .github/workflows/release.yml).
#
# The route follows the committed release-team-id file:
#   UNSIGNED-ADHOC  the unsigned route: Tauri signs the app ad-hoc (APPLE_SIGNING_IDENTITY=-), nothing is
#                   notarized, no Apple account or certificate is used. Needs only the updater key below.
#   a Team ID       the signed route, on the owner's Mac with the Developer ID certificate in the login
#                   Keychain (no .p12 needed).
#
# Usage: scripts/build-mac-local.sh
# Needs, exported in this shell (the notarization variables on the signed route only):
#   notarization: APPLE_API_KEY + APPLE_API_ISSUER + APPLE_API_KEY_PATH,
#                 or APPLE_ID + APPLE_PASSWORD (+ APPLE_TEAM_ID)
#   updater key:  TAURI_SIGNING_PRIVATE_KEY (the key text or the key file's path)
#                 and TAURI_SIGNING_PRIVATE_KEY_PASSWORD
# Optional: APPLE_SIGNING_IDENTITY (else the one "Developer ID Application"
#   identity found by `security find-identity -v -p codesigning`), APPLE_TEAM_ID
#   (else taken from the identity's "(TEAMID)").
# Output: target/release-assets/ReTV-Bridge-<v>-mac-universal.{dmg,app.tar.gz,app.tar.gz.sig}
# Env (tests): RELEASE_TEAM_ID_FILE (default release-team-id).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

die() { echo "build-mac-local: $*" >&2; exit 1; }
PRINT_IDENTITY=0
case "${1:-}" in
  "") ;;
  --print-identity) PRINT_IDENTITY=1 ;; # print the identity and team id (ad-hoc: "-" and UNSIGNED-ADHOC), then stop (tests)
  --print-rustflags) ;;                  # print the rustc flags of the build, one per line, then stop (tests)
  *) echo "usage: $0 [--print-identity | --print-rustflags]" >&2; exit 2 ;;
esac
[ "$(uname -s)" = Darwin ] || die "run this on macOS"

# Exports CARGO_ENCODED_RUSTFLAGS (flags separated by 0x1f) with the --remap-path-prefix flags; RUSTFLAGS would be ignored.
remap_flags() {
  local flags
  flags="$("$ROOT/scripts/remap-path-flags.sh" "$ROOT" | tr '\n' '\037')" || die "remap-path-flags.sh failed"
  [ -n "$flags" ] || die "remap-path-flags.sh printed nothing"
  export CARGO_ENCODED_RUSTFLAGS="${flags%$'\037'}"
  unset RUSTFLAGS
}
if [ "${1:-}" = --print-rustflags ]; then
  remap_flags
  printf '%s\n' "$CARGO_ENCODED_RUSTFLAGS" | tr '\037' '\n'
  exit 0
fi

AD_HOC_MARK="UNSIGNED-ADHOC"
TEAM_FILE="${RELEASE_TEAM_ID_FILE:-$ROOT/release-team-id}"
COMMITTED_TEAM="$( { grep -v '^[[:space:]]*#' "$TEAM_FILE" 2>/dev/null || true; } | tr -d '[:space:]')"
FINISH_ARGS=()
if [ "$COMMITTED_TEAM" = "$AD_HOC_MARK" ]; then
  # The unsigned route: whatever the shell has exported, Tauri gets no certificate, no team and no notarization
  # credentials (it would try to notarize with them), only the ad-hoc identity "-".
  unset APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_TEAM_ID APPLE_ID APPLE_PASSWORD \
    APPLE_API_KEY APPLE_API_ISSUER APPLE_API_KEY_PATH
  export APPLE_SIGNING_IDENTITY="-"
  FINISH_ARGS+=(--ad-hoc)
  if [ "$PRINT_IDENTITY" -eq 1 ]; then
    printf '%s\n%s\n' "$APPLE_SIGNING_IDENTITY" "$AD_HOC_MARK"
    exit 0
  fi
else
  if [ -z "${APPLE_SIGNING_IDENTITY:-}" ]; then
    IDS="$(security find-identity -v -p codesigning | sed -n 's/^ *[0-9][0-9]*) [0-9A-F]* "\(Developer ID Application: .*\)"$/\1/p')"
    COUNT="$(printf '%s' "$IDS" | grep -c . || true)"
    [ "$COUNT" -eq 1 ] || die "found $COUNT Developer ID Application identities; export APPLE_SIGNING_IDENTITY to pick one"
    APPLE_SIGNING_IDENTITY="$IDS"
  fi
  export APPLE_SIGNING_IDENTITY
  if [ -z "${APPLE_TEAM_ID:-}" ]; then
    APPLE_TEAM_ID="$(printf '%s' "$APPLE_SIGNING_IDENTITY" | sed -n 's/.*(\([A-Z0-9]\{10\}\))$/\1/p')"
    [ -n "$APPLE_TEAM_ID" ] || die "cannot read the team id from \"$APPLE_SIGNING_IDENTITY\"; export APPLE_TEAM_ID"
  fi
  export APPLE_TEAM_ID
  if [ "$PRINT_IDENTITY" -eq 1 ]; then
    printf '%s\n%s\n' "$APPLE_SIGNING_IDENTITY" "$APPLE_TEAM_ID"
    exit 0
  fi
fi
if [ "${#FINISH_ARGS[@]}" -eq 0 ]; then
  if [ -f "$TEAM_FILE" ] && [ "$COMMITTED_TEAM" != "$APPLE_TEAM_ID" ]; then
    die "release-team-id says $COMMITTED_TEAM, the identity's team is $APPLE_TEAM_ID"
  fi
  # mac-finish.sh checks the notarization variables in detail; fail early here.
  if [ -z "$(printenv APPLE_API_KEY || true)" ] \
    && { [ -z "$(printenv APPLE_ID || true)" ] || [ -z "$(printenv APPLE_PASSWORD || true)" ]; }; then
    die "export APPLE_API_KEY, APPLE_API_ISSUER, APPLE_API_KEY_PATH (or APPLE_ID, APPLE_PASSWORD) for notarization"
  fi
fi
[ -n "$(printenv TAURI_SIGNING_PRIVATE_KEY || true)" ] || die "export TAURI_SIGNING_PRIVATE_KEY (and _PASSWORD) for the updater signature"

VERSION="$(perl -MJSON::PP -e 'local $/; open(my $h, "<", $ARGV[0]) or die; print JSON::PP->new->decode(<$h>)->{version}' app/tauri.conf.json)"
# Same gate as the CI test job: release notes and a real updater pubkey.
"$ROOT/scripts/release-version.sh" "bridge-v$VERSION" "$ROOT" > /dev/null
if [ "${#FINISH_ARGS[@]}" -gt 0 ]; then
  echo "ReTV Bridge $VERSION, ad-hoc signed, not notarized (release-team-id says $AD_HOC_MARK)"
else
  echo "ReTV Bridge $VERSION, signing as $APPLE_SIGNING_IDENTITY"
fi

rustup target add aarch64-apple-darwin x86_64-apple-darwin
# No path of this Mac in the binaries: the home folder (it names the account), the cargo registry, the toolchain and
# this checkout become /build/… (scripts/remap-path-flags.sh). CARGO_ENCODED_RUSTFLAGS keeps paths with spaces whole.
remap_flags
cargo tauri build --target universal-apple-darwin -- --locked

OUT="$ROOT/target/release-assets"
rm -rf "$OUT"
"$ROOT/scripts/mac-finish.sh" --version "$VERSION" \
  --bundle-dir "$ROOT/target/universal-apple-darwin/release/bundle" --out "$OUT" ${FINISH_ARGS[@]+"${FINISH_ARGS[@]}"}
