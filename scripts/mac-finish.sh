#!/usr/bin/env bash
# After `cargo tauri build --target universal-apple-darwin`: notarize and
# staple the DMG, verify the signatures, and copy the release assets out under
# their published names. Used by .github/workflows/release.yml and by
# scripts/build-mac-local.sh.
#
# Tauri already signs, notarizes and staples the .app and signs the DMG; the
# DMG itself still needs its own notarization ticket for `spctl -t install`.
#
# With --ad-hoc (the unsigned route: no Apple Developer ID, release-team-id says UNSIGNED-ADHOC) Tauri has signed
# the app ad-hoc (APPLE_SIGNING_IDENTITY=-) and nothing is notarized: the checks are codesign --verify --deep --strict
# and an ad-hoc signature without a TeamIdentifier, on the .app and on the app inside .app.tar.gz. No notarytool,
# stapler or spctl, and no Apple variables are read.
#
# Usage: scripts/mac-finish.sh --version <v> --bundle-dir <dir> --out <dir> [--ad-hoc]
#   <dir> is target/universal-apple-darwin/release/bundle
# Env (signed routes only): APPLE_TEAM_ID (required: the expected TeamIdentifier), and either
#   APPLE_API_KEY + APPLE_API_ISSUER + APPLE_API_KEY_PATH (App Store Connect key), or
#   APPLE_ID + APPLE_PASSWORD (app-specific password).
# Writes ReTV-Bridge-<v>-mac-universal.{dmg,app.tar.gz,app.tar.gz.sig} to --out,
# and only once every check has passed.
set -euo pipefail

VERSION="" BUNDLE="" OUT="" AD_HOC=0
while [ $# -gt 0 ]; do
  case "$1" in
    --version) VERSION="$2"; shift ;;
    --bundle-dir) BUNDLE="$2"; shift ;;
    --out) OUT="$2"; shift ;;
    --ad-hoc) AD_HOC=1 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done
[ -n "$VERSION" ] && [ -n "$BUNDLE" ] && [ -n "$OUT" ] || {
  echo "usage: $0 --version <v> --bundle-dir <dir> --out <dir> [--ad-hoc]" >&2; exit 2; }

die() { echo "mac-finish: $*" >&2; exit 1; }

# exactly one match of a glob, else fail
one() {
  local what="$1"; shift
  local found=()
  for f in "$@"; do [ -e "$f" ] && found+=("$f"); done
  [ "${#found[@]}" -eq 1 ] || die "expected one $what, found ${#found[@]}"
  printf '%s\n' "${found[0]}"
}

DMG="$(one "DMG in $BUNDLE/dmg" "$BUNDLE"/dmg/*.dmg)"
APP="$(one ".app in $BUNDLE/macos" "$BUNDLE"/macos/*.app)"
TAR="$(one "updater .app.tar.gz in $BUNDLE/macos" "$BUNDLE"/macos/*.app.tar.gz)"
SIG="$TAR.sig"
[ -f "$SIG" ] || die "missing updater signature $SIG (is TAURI_SIGNING_PRIVATE_KEY set?)"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
# The updater installs the app from .app.tar.gz, not from the DMG: every check on the app is made on that copy too.
mkdir -p "$WORK/from-tar"
tar -xzf "$TAR" -C "$WORK/from-tar" || die "cannot unpack $TAR"
TAR_APP="$WORK/from-tar/$(basename "$APP")"
[ -d "$TAR_APP" ] || die "$TAR does not hold $(basename "$APP")"

if [ "$AD_HOC" -eq 1 ]; then
  # ad-hoc: a valid signature (Apple Silicon runs no unsigned arm64 code) that names no team and no person
  check_ad_hoc() { # <.app> <label>
    codesign --verify --deep --strict --verbose=2 "$1" || die "codesign --verify failed on $2"
    local info
    info="$(codesign -dv "$1" 2>&1)"
    printf '%s\n' "$info" | grep -qx 'Signature=adhoc' || die "$2 is not ad-hoc signed"
    local team
    team="$(printf '%s\n' "$info" | sed -n 's/^TeamIdentifier=//p')"
    [ -z "$team" ] || [ "$team" = "not set" ] || die "$2 carries TeamIdentifier $team, an ad-hoc build carries none"
  }
  echo "verifying (ad-hoc signed, not notarized)"
  check_ad_hoc "$APP" "$APP"
  check_ad_hoc "$TAR_APP" "the app inside $TAR"
else
  TEAM="${APPLE_TEAM_ID:-}"
  [ -n "$TEAM" ] || die "APPLE_TEAM_ID is not set (the TeamIdentifier the app must carry)"

  key_id="$(printenv APPLE_API_KEY || true)"
  issuer="$(printenv APPLE_API_ISSUER || true)"
  p8_file="$(printenv APPLE_API_KEY_PATH || true)"
  apple_id="$(printenv APPLE_ID || true)"
  apple_pw="$(printenv APPLE_PASSWORD || true)"
  if [ -n "$key_id" ]; then
    [ -n "$issuer" ] || die "APPLE_API_KEY is set but APPLE_API_ISSUER is not"
    [ -n "$p8_file" ] && [ -f "$p8_file" ] || die "APPLE_API_KEY_PATH must name the .p8 file"
    notary=(--key "$p8_file" --key-id "$key_id" --issuer "$issuer")
  elif [ -n "$apple_id" ] && [ -n "$apple_pw" ]; then
    notary=(--apple-id "$apple_id" --password "$apple_pw" --team-id "$TEAM")
  else
    die "no notarization credentials (APPLE_API_KEY, APPLE_API_ISSUER, APPLE_API_KEY_PATH or APPLE_ID, APPLE_PASSWORD)"
  fi

  echo "notarizing $DMG"
  xcrun notarytool submit "$DMG" "${notary[@]}" --wait || die "notarization of the DMG failed"
  xcrun stapler staple "$DMG" || die "stapling the DMG failed"

  echo "verifying"
  codesign --verify --deep --strict --verbose=2 "$APP" || die "codesign --verify failed on $APP"
  APP_TEAM="$(codesign -dv "$APP" 2>&1 | sed -n 's/^TeamIdentifier=//p')"
  [ "$APP_TEAM" = "$TEAM" ] || die "app TeamIdentifier is ${APP_TEAM:-missing}, expected $TEAM"
  spctl -a -vv -t install "$DMG" || die "spctl rejects $DMG"
  xcrun stapler validate "$DMG" || die "stapler validate failed on $DMG"

  codesign --verify --deep --strict --verbose=2 "$TAR_APP" || die "codesign --verify failed on the app inside $TAR"
  xcrun stapler validate "$TAR_APP" || die "stapler validate failed on the app inside $TAR"
fi

mkdir -p "$OUT"
BASE="ReTV-Bridge-$VERSION-mac-universal"
cp "$DMG" "$OUT/$BASE.dmg"
cp "$TAR" "$OUT/$BASE.app.tar.gz"
cp "$SIG" "$OUT/$BASE.app.tar.gz.sig"
echo "assets in $OUT:"
ls -l "$OUT"
