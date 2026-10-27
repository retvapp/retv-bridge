#!/usr/bin/env bash
# Tests for the release helpers (release-version.sh, mac-finish.sh).
# Apple's tools are faked through PATH shims, so this runs on any OS:
#   bash scripts/test-release-scripts.sh
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
GOOD=0
FAILED=0

ok() { GOOD=$((GOOD + 1)); echo "ok   $1"; }
bad() { FAILED=$((FAILED + 1)); echo "FAIL $1"; [ -n "${2:-}" ] && sed 's/^/     /' "$2"; }

# --- release-version.sh ------------------------------------------------------

# A fake bridge root with tauri.conf.json version $1 and notes for version $2
# (notes JSON $3, default en+ru).
DEFAULT_NOTES='{"ru": ["Первая версия"], "en": ["First release"]}'
FAKE_PUBKEY="dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEZBS0UK"
make_root() {
  local root="$TMP/root-$RANDOM$RANDOM"
  mkdir -p "$root/app" "$root/release-notes"
  # CASE_PUBKEY: the updater pubkey to write ("none" leaves plugins.updater out)
  if [ "${CASE_PUBKEY:-$FAKE_PUBKEY}" = none ]; then
    printf '{\n  "productName": "ReTV Bridge",\n  "version": "%s"\n}\n' "$1" > "$root/app/tauri.conf.json"
  else
    printf '{\n  "productName": "ReTV Bridge",\n  "version": "%s",\n  "plugins": {"updater": {"pubkey": "%s"}}\n}\n' \
      "$1" "${CASE_PUBKEY:-$FAKE_PUBKEY}" > "$root/app/tauri.conf.json"
  fi
  if [ -n "$2" ]; then
    printf '%s\n' "${3:-$DEFAULT_NOTES}" > "$root/release-notes/$2.json"
  fi
  # The Rust side: [workspace.package] version (CASE_CARGO_VERSION) and the workspace crates in Cargo.lock
  # (CASE_LOCK_VERSION), the same as tauri.conf.json unless a case says otherwise.
  printf '[workspace]\nmembers = ["app"]\n\n[workspace.package]\nversion = "%s"\nedition = "2021"\n\n[workspace.dependencies]\nserde = { version = "=1.0.229" }\n' \
    "${CASE_CARGO_VERSION:-$1}" > "$root/Cargo.toml"
  printf 'version = 4\n\n[[package]]\nname = "retv-bridge-app"\nversion = "%s"\ndependencies = [\n "serde",\n]\n\n[[package]]\nname = "retv-bridge-core"\nversion = "%s"\n\n[[package]]\nname = "serde"\nversion = "1.0.229"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\n' \
    "${CASE_LOCK_VERSION:-$1}" "${CASE_LOCK_VERSION:-$1}" > "$root/Cargo.lock"
  echo "$root"
}

version_case() { # name expect(ok|fail) tag conf-version notes-version [notes-json] [expected stdout]
  local name="$1" expect="$2" tag="$3" root out rc
  root="$(make_root "$4" "$5" "${6:-}")"
  out="$TMP/out.txt"
  bash "$HERE/release-version.sh" "$tag" "$root" > "$out" 2>&1
  rc=$?
  if [ "$expect" = ok ]; then
    if [ "$rc" -eq 0 ] && [ "$(cat "$out")" = "${7:-}" ]; then ok "$name"; else bad "$name (rc=$rc)" "$out"; fi
  else
    if [ "$rc" -ne 0 ]; then ok "$name"; else bad "$name (expected failure)" "$out"; fi
  fi
}

version_case "tag equal to tauri.conf version prints it" ok bridge-v1.2.3 1.2.3 1.2.3 "" 1.2.3
version_case "pre-release tag" ok bridge-v0.9.0-rc.1 0.9.0-rc.1 0.9.0-rc.1 "" 0.9.0-rc.1
version_case "refs/tags/ prefix accepted" ok refs/tags/bridge-v1.0.0 1.0.0 1.0.0 "" 1.0.0
version_case "tag differs from tauri.conf version" fail bridge-v1.0.1 1.0.0 1.0.1
version_case "tag without the bridge-v prefix" fail v1.0.0 1.0.0 1.0.0
version_case "tag not semver" fail bridge-v1.0 1.0 1.0
version_case "leading zero" fail bridge-v1.02.0 1.02.0 1.02.0
version_case "release notes missing" fail bridge-v1.0.0 1.0.0 ""
version_case "release notes without ru" fail bridge-v1.0.0 1.0.0 1.0.0 '{"en": ["First"]}'
version_case "release notes with empty en" fail bridge-v1.0.0 1.0.0 1.0.0 '{"en": [], "ru": ["Первая"]}'
version_case "release notes not JSON" fail bridge-v1.0.0 1.0.0 1.0.0 'en: First'
# The rules the staging script's manifest applies (marketNotes, the manifest's version pattern), so a tag fails here
# and not after a full signed build.
NINE='["1", "2", "3", "4", "5", "6", "7", "8", "9"]'
LONG="$(printf 'x%.0s' $(seq 1 201))"
version_case "release notes with 9 lines" fail bridge-v1.0.0 1.0.0 1.0.0 "{\"en\": $NINE, \"ru\": [\"Первая\"]}"
version_case "release notes with 8 lines" ok bridge-v1.0.0 1.0.0 1.0.0 '{"en": ["1","2","3","4","5","6","7","8"], "ru": ["Первая"]}' 1.0.0
version_case "a release-notes line over 200 characters" fail bridge-v1.0.0 1.0.0 1.0.0 "{\"en\": [\"$LONG\"], \"ru\": [\"Первая\"]}"
version_case "200 Cyrillic characters are 200, not 400" ok bridge-v1.0.0 1.0.0 1.0.0 "{\"en\": [\"First\"], \"ru\": [\"$(printf 'ж%.0s' $(seq 1 200))\"]}" 1.0.0
version_case "a blank release-notes line" fail bridge-v1.0.0 1.0.0 1.0.0 '{"en": ["  "], "ru": ["Первая"]}'
version_case "Cyrillic in the en notes (they ship on retv.app)" fail bridge-v1.0.0 1.0.0 1.0.0 '{"en": ["Faster Wi‑Fi поиск"], "ru": ["Первая"]}'
version_case "a bad optional uk list" fail bridge-v1.0.0 1.0.0 1.0.0 '{"en": ["First"], "ru": ["Первая"], "uk": []}'
version_case "a good uk list" ok bridge-v1.0.0 1.0.0 1.0.0 '{"en": ["First"], "ru": ["Первая"], "uk": ["Перша"]}' 1.0.0
version_case "a pre-release with an inner hyphen (the manifest refuses it)" fail bridge-v1.0.0-rc-1 1.0.0-rc-1 1.0.0-rc-1
# The updater pubkey placeholder would ship a build that never updates.
CASE_PUBKEY=REPLACE_WITH_THE_RETV_BRIDGE_UPDATER_PUBLIC_KEY \
  version_case "updater pubkey still the placeholder" fail bridge-v1.0.0 1.0.0 1.0.0
CASE_PUBKEY=none version_case "no updater pubkey" fail bridge-v1.0.0 1.0.0 1.0.0
CASE_PUBKEY=" " version_case "blank updater pubkey" fail bridge-v1.0.0 1.0.0 1.0.0
out="$TMP/pk.out"
CASE_PUBKEY=REPLACE_WITH_THE_RETV_BRIDGE_UPDATER_PUBLIC_KEY
bash "$HERE/release-version.sh" bridge-v1.0.0 "$(make_root 1.0.0 1.0.0)" > "$out" 2>&1
unset CASE_PUBKEY
if grep -q "pubkey" "$out" && grep -q "ключ" "$out"; then ok "  placeholder message in English and Russian"; else bad "  placeholder message in English and Russian" "$out"; fi

# One version everywhere: hello, the User-Agent and the updater read the Cargo version (CARGO_PKG_VERSION).
CASE_CARGO_VERSION=0.1.0 version_case "Cargo.toml workspace version differs from tauri.conf.json" fail bridge-v1.0.0 1.0.0 1.0.0
CASE_LOCK_VERSION=0.1.0 version_case "Cargo.lock workspace crates differ from tauri.conf.json" fail bridge-v1.0.0 1.0.0 1.0.0
out="$TMP/cargo.out"
CASE_CARGO_VERSION=0.1.0
bash "$HERE/release-version.sh" bridge-v1.0.0 "$(make_root 1.0.0 1.0.0)" > "$out" 2>&1
unset CASE_CARGO_VERSION
grep -q "Cargo.toml" "$out" && grep -q "release-version.sh --set" "$out" && ok "  says which file and how to fix it" || bad "  says which file and how to fix it" "$out"

# --set <version>: tauri.conf.json, Cargo.toml and Cargo.lock in one go; registry crates untouched.
root="$(CASE_CARGO_VERSION=0.1.0 CASE_LOCK_VERSION=0.1.0 make_root 0.1.0 "")"
printf '%s\n' "$DEFAULT_NOTES" > "$root/release-notes/1.2.0-rc.1.json"
if bash "$HERE/release-version.sh" --set 1.2.0-rc.1 "$root" > "$out" 2>&1 \
  && [ "$(bash "$HERE/release-version.sh" bridge-v1.2.0-rc.1 "$root" 2>&1)" = 1.2.0-rc.1 ]; then
  ok "--set bumps tauri.conf.json, Cargo.toml and Cargo.lock together"
else
  bad "--set bumps tauri.conf.json, Cargo.toml and Cargo.lock together" "$out"
fi
grep -q '"productName": "ReTV Bridge"' "$root/app/tauri.conf.json" && grep -q 'edition = "2021"' "$root/Cargo.toml" \
  && grep -q '^version = "1.0.229"$' "$root/Cargo.lock" && [ "$(grep -c '^version = "1.2.0-rc.1"$' "$root/Cargo.lock")" = 2 ] \
  && ok "  only the versions change" || bad "  only the versions change" "$root/Cargo.lock"
bash "$HERE/release-version.sh" --set 1.2 "$root" > "$out" 2>&1 && bad "--set refuses a version that is not semver" || ok "--set refuses a version that is not semver"

# --- mac-finish.sh -----------------------------------------------------------

# Shims for xcrun, codesign and spctl: each call is logged; FAIL_ON is a glob
# over " tool args " (e.g. "xcrun stapler validate *from-tar*") that exits 1; FAKE_TEAM is what codesign -dv reports.
SHIMS="$TMP/shims"
mkdir -p "$SHIMS"
for tool in xcrun codesign spctl; do
  cat > "$SHIMS/$tool" <<EOF
#!/usr/bin/env bash
echo "$tool \$*" >> "\$SHIM_LOG"
case " $tool \$* " in
  *" "\${FAIL_ON:-no-such-call}" "*) echo "$tool: refused" >&2; exit 1 ;;
esac
if [ "$tool" = codesign ] && [ "\$1" = "-dv" ]; then
  echo "Executable=x" >&2
  adhoc="\${FAKE_ADHOC:-}"
  case " $tool \$* " in *" "\${DEVID_ON:-no-such-call}" "*) adhoc="" ;; esac
  if [ -n "\$adhoc" ]; then
    echo "Signature=adhoc" >&2
    echo "TeamIdentifier=not set" >&2
  else
    echo "TeamIdentifier=\${FAKE_TEAM:-ABCDE12345}" >&2
  fi
fi
exit 0
EOF
  chmod +x "$SHIMS/$tool"
done

# A fake tauri bundle dir for version $1 (the .app.tar.gz is a real archive of
# the .app). Extra args: "two-dmg", "no-sig", "tar-without-app".
make_bundle() {
  local b="$TMP/bundle-$RANDOM$RANDOM" v="$1"; shift
  mkdir -p "$b/dmg" "$b/macos/ReTV Bridge.app/Contents"
  echo dmg > "$b/dmg/ReTV Bridge_${v}_universal.dmg"
  (cd "$b/macos" && tar -czf "ReTV Bridge.app.tar.gz" "ReTV Bridge.app")
  echo sig > "$b/macos/ReTV Bridge.app.tar.gz.sig"
  for x in "$@"; do
    case "$x" in
      two-dmg) echo dmg > "$b/dmg/ReTV Bridge_${v}_aarch64.dmg" ;;
      no-sig) rm "$b/macos/ReTV Bridge.app.tar.gz.sig" ;;
      tar-without-app) (cd "$b/macos" && mkdir -p other && tar -czf "ReTV Bridge.app.tar.gz" other) ;;
    esac
  done
  echo "$b"
}

# run_finish <name> <expect ok|fail> [env assignments...] -- [extra bundle args]
run_finish() {
  local name="$1" expect="$2"; shift 2
  local envs=() extra=()
  while [ $# -gt 0 ] && [ "$1" != "--" ]; do envs+=("$1"); shift; done
  [ "${1:-}" = "--" ] && shift
  extra=("$@")
  local bundle out log rc
  bundle="$(make_bundle 1.2.3 ${extra[@]+"${extra[@]}"})"
  out="$TMP/dist-$RANDOM$RANDOM"
  log="$TMP/log-$RANDOM$RANDOM"
  : > "$log"
  env -i PATH="$SHIMS:/usr/bin:/bin" HOME="$TMP" SHIM_LOG="$log" ${envs[@]+"${envs[@]}"} \
    bash "$HERE/mac-finish.sh" --version 1.2.3 --bundle-dir "$bundle" --out "$out" ${FINISH_FLAGS[@]+"${FINISH_FLAGS[@]}"} > "$log.out" 2>&1
  rc=$?
  LAST_LOG="$log"; LAST_OUT="$out"; LAST_RC=$rc
  if [ "$expect" = ok ]; then
    [ "$rc" -eq 0 ] || { bad "$name (rc=$rc)" "$log.out"; return 1; }
  else
    [ "$rc" -ne 0 ] || { bad "$name (expected failure)" "$log.out"; return 1; }
    # nothing reaches the output dir after a failure
    if [ -d "$out" ] && [ -n "$(ls -A "$out")" ]; then bad "$name (left files in the output dir)"; return 1; fi
  fi
  ok "$name"
}

FAKE_KEY_ID=KEYID123
FAKE_PW=app-specific
FAKE_P8="$TMP/AuthKey.p8"
FAKE_MISSING_P8="$TMP/missing.p8"
echo fake > "$FAKE_P8"
# One assignment per line (the publish leak check judges each one on its own).
API=(
  APPLE_API_KEY=$FAKE_KEY_ID
  APPLE_API_ISSUER=issuer-uuid
  APPLE_API_KEY_PATH=$FAKE_P8
  APPLE_TEAM_ID=ABCDE12345
)
IDS=(
  APPLE_ID=dev@example.com
  APPLE_PASSWORD=$FAKE_PW
  APPLE_TEAM_ID=ABCDE12345
)
API_NO_P8=(
  APPLE_API_KEY=$FAKE_KEY_ID
  APPLE_API_ISSUER=issuer-uuid
  APPLE_TEAM_ID=ABCDE12345
)
API_NO_TEAM=(
  APPLE_API_KEY=$FAKE_KEY_ID
  APPLE_API_ISSUER=issuer-uuid
  APPLE_API_KEY_PATH=$FAKE_P8
)
API_MISSING_P8=(
  APPLE_API_KEY=$FAKE_KEY_ID
  APPLE_API_ISSUER=issuer-uuid
  APPLE_API_KEY_PATH=$FAKE_MISSING_P8
  APPLE_TEAM_ID=ABCDE12345
)

if run_finish "happy path with an API key" ok "${API[@]}"; then
  for f in ReTV-Bridge-1.2.3-mac-universal.dmg ReTV-Bridge-1.2.3-mac-universal.app.tar.gz ReTV-Bridge-1.2.3-mac-universal.app.tar.gz.sig; do
    [ -f "$LAST_OUT/$f" ] && ok "  renamed: $f" || bad "  renamed: $f missing"
  done
  [ "$(ls "$LAST_OUT" | wc -l | tr -d ' ')" = 3 ] && ok "  exactly three assets" || bad "  exactly three assets"
  grep -q "^xcrun notarytool submit .*ReTV Bridge_1.2.3_universal.dmg --key $TMP/AuthKey.p8 --key-id KEYID123 --issuer issuer-uuid --wait" "$LAST_LOG" \
    && ok "  DMG notarized with the API key" || bad "  DMG notarized with the API key" "$LAST_LOG"
  grep -q "^xcrun stapler staple " "$LAST_LOG" && ok "  DMG stapled" || bad "  DMG stapled" "$LAST_LOG"
  grep -q "^codesign --verify --deep --strict .*ReTV Bridge.app" "$LAST_LOG" && ok "  codesign --verify --deep --strict on the app" || bad "  codesign verify" "$LAST_LOG"
  grep -q "^spctl -a -vv -t install .*\.dmg" "$LAST_LOG" && ok "  spctl install check on the DMG" || bad "  spctl" "$LAST_LOG"
  grep -q "^xcrun stapler validate .*\.dmg" "$LAST_LOG" && ok "  stapler validate on the DMG" || bad "  stapler validate" "$LAST_LOG"
  # verification only after stapling
  staple_line=$(grep -n "stapler staple" "$LAST_LOG" | cut -d: -f1)
  spctl_line=$(grep -n "^spctl" "$LAST_LOG" | cut -d: -f1)
  [ "$staple_line" -lt "$spctl_line" ] && ok "  verified after stapling" || bad "  verified after stapling" "$LAST_LOG"
  # the updater archive is what auto-update installs: its app is checked too
  grep -q "^codesign --verify --deep --strict .*from-tar/ReTV Bridge.app" "$LAST_LOG" \
    && ok "  codesign --verify on the app inside .app.tar.gz" || bad "  codesign on the archived app" "$LAST_LOG"
  grep -q "^xcrun stapler validate .*from-tar/ReTV Bridge.app" "$LAST_LOG" \
    && ok "  stapler validate on the app inside .app.tar.gz" || bad "  stapler on the archived app" "$LAST_LOG"
fi

if run_finish "Apple ID notarization when no API key" ok "${IDS[@]}"; then
  grep -q -- "--apple-id dev@example.com --password app-specific --team-id ABCDE12345 --wait" "$LAST_LOG" \
    && ok "  notarytool with the Apple ID" || bad "  notarytool with the Apple ID" "$LAST_LOG"
fi
run_finish "no notarization credentials" fail APPLE_TEAM_ID=ABCDE12345
run_finish "API key without its .p8 path" fail "${API_NO_P8[@]}"
run_finish "notarization rejected" fail "${API[@]}" "FAIL_ON=xcrun notarytool"
run_finish "staple fails" fail "${API[@]}" "FAIL_ON=xcrun stapler staple"
run_finish "codesign verify fails" fail "${API[@]}" "FAIL_ON=codesign --verify"
run_finish "spctl rejects the DMG" fail "${API[@]}" "FAIL_ON=spctl -a"
run_finish "stapler validate fails" fail "${API[@]}" "FAIL_ON=xcrun stapler validate"
run_finish "team id differs" fail "${API[@]}" FAKE_TEAM=ZZZZZ99999
run_finish "no team id to compare with" fail "${API_NO_TEAM[@]}"
run_finish "API key path that does not exist" fail "${API_MISSING_P8[@]}"
run_finish "two DMGs in the bundle dir" fail "${API[@]}" -- two-dmg
run_finish "updater signature missing" fail "${API[@]}" -- no-sig
run_finish "archived app not stapled" fail "${API[@]}" "FAIL_ON=xcrun stapler validate *from-tar*"
run_finish "archived app fails codesign" fail "${API[@]}" "FAIL_ON=codesign --verify --deep --strict *from-tar*"
run_finish "updater archive without the app" fail "${API[@]}" -- tar-without-app
# An unset GitHub secret arrives as an empty variable: it must not pick the API route.
IDS_EMPTY_KEY=(
  "${IDS[@]}"
  APPLE_API_KEY=""
  APPLE_API_ISSUER=""
)
if run_finish "empty APPLE_API_KEY falls back to the Apple ID" ok "${IDS_EMPTY_KEY[@]}"; then
  grep -q -- "--apple-id dev@example.com" "$LAST_LOG" \
    && ok "  notarytool with the Apple ID" || bad "  notarytool with the Apple ID" "$LAST_LOG"
fi

# --- mac-finish.sh --ad-hoc (the unsigned route) -------------------------------

FINISH_FLAGS=(--ad-hoc)
# No Apple variable at all: the unsigned route needs none.
if run_finish "ad-hoc: happy path without any Apple credentials" ok FAKE_ADHOC=1; then
  for f in ReTV-Bridge-1.2.3-mac-universal.dmg ReTV-Bridge-1.2.3-mac-universal.app.tar.gz ReTV-Bridge-1.2.3-mac-universal.app.tar.gz.sig; do
    [ -f "$LAST_OUT/$f" ] && ok "  renamed: $f" || bad "  renamed: $f missing"
  done
  [ "$(ls "$LAST_OUT" | wc -l | tr -d ' ')" = 3 ] && ok "  exactly three assets" || bad "  exactly three assets"
  grep -q "^codesign --verify --deep --strict .*/macos/ReTV Bridge.app" "$LAST_LOG" \
    && ok "  codesign --verify --deep --strict on the app" || bad "  codesign verify on the app" "$LAST_LOG"
  grep -q "^codesign --verify --deep --strict .*from-tar/ReTV Bridge.app" "$LAST_LOG" \
    && ok "  codesign --verify --deep --strict on the app inside .app.tar.gz" || bad "  codesign on the archived app" "$LAST_LOG"
  grep -q "^codesign -dv .*from-tar/ReTV Bridge.app" "$LAST_LOG" \
    && ok "  the archived app's signature kind is read" || bad "  codesign -dv on the archived app" "$LAST_LOG"
  ! grep -qE "^(xcrun|spctl) " "$LAST_LOG" \
    && ok "  no notarytool, stapler or spctl" || bad "  no notarytool, stapler or spctl" "$LAST_LOG"
fi
run_finish "ad-hoc: a Developer ID signed app is refused" fail
grep -q "is not ad-hoc signed" "$LAST_LOG.out" && ok "  says it is not ad-hoc signed" || bad "  says it is not ad-hoc signed" "$LAST_LOG.out"
run_finish "ad-hoc: archived app signed with a Developer ID" fail FAKE_ADHOC=1 "DEVID_ON=codesign -dv *from-tar*"
run_finish "ad-hoc: codesign verify fails" fail FAKE_ADHOC=1 "FAIL_ON=codesign --verify"
run_finish "ad-hoc: archived app fails codesign" fail FAKE_ADHOC=1 "FAIL_ON=codesign --verify --deep --strict *from-tar*"
run_finish "ad-hoc: updater signature missing" fail FAKE_ADHOC=1 -- no-sig
run_finish "ad-hoc: updater archive without the app" fail FAKE_ADHOC=1 -- tar-without-app
FINISH_FLAGS=()
# Without --ad-hoc an ad-hoc app never passes the signed route's Team ID pin.
run_finish "signed route refuses an ad-hoc app" fail "${API[@]}" FAKE_ADHOC=1

# --- build-mac-local.sh: identity and team id --------------------------------

# `security find-identity` prints FAKE_IDENTITIES; uname says Darwin.
cat > "$SHIMS/security" <<'EOF'
#!/usr/bin/env bash
printf '%b' "${FAKE_IDENTITIES:-}"
EOF
cat > "$SHIMS/uname" <<'EOF'
#!/usr/bin/env bash
echo Darwin
EOF
chmod +x "$SHIMS/security" "$SHIMS/uname"
DEVID='  2) 1123456789ABCDEF0123456789ABCDEF01234567 "Developer ID Application: Some Body (ABCDE12345)"\n'
DEVID2='  3) 2123456789ABCDEF0123456789ABCDEF01234567 "Developer ID Application: Other Org (ZZZZZ99999)"\n'
APPLEDEV='  1) 0123456789ABCDEF0123456789ABCDEF01234567 "Apple Development: Some Body (AAAAAAAAAA)"\n'
FOOTER='     2 valid identities found\n'

identity_case() { # name expect(ok|fail) expected-stdout [env...]
  local name="$1" expect="$2" want="$3" out rc; shift 3
  out="$TMP/id.out"
  # The signed route unless a case names a release-team-id file (the repo's own file must not decide a test).
  env -i PATH="$SHIMS:/usr/bin:/bin" HOME="$TMP" RELEASE_TEAM_ID_FILE="$TMP/no-release-team-id" "$@" \
    bash "$HERE/build-mac-local.sh" --print-identity > "$out" 2>&1
  rc=$?
  if [ "$expect" = ok ]; then
    if [ "$rc" -eq 0 ] && [ "$(cat "$out")" = "$want" ]; then ok "$name"; else bad "$name (rc=$rc)" "$out"; fi
  else
    if [ "$rc" -ne 0 ]; then ok "$name"; else bad "$name (expected failure)" "$out"; fi
  fi
}
identity_case "picks the one Developer ID identity, team id from it" ok \
  "$(printf 'Developer ID Application: Some Body (ABCDE12345)\nABCDE12345')" \
  "FAKE_IDENTITIES=$APPLEDEV$DEVID$FOOTER"
identity_case "no Developer ID identity" fail "" "FAKE_IDENTITIES=$APPLEDEV$FOOTER"
identity_case "two Developer ID identities need APPLE_SIGNING_IDENTITY" fail "" "FAKE_IDENTITIES=$DEVID$DEVID2$FOOTER"
identity_case "APPLE_SIGNING_IDENTITY picks one of two" ok \
  "$(printf 'Developer ID Application: Other Org (ZZZZZ99999)\nZZZZZ99999')" \
  "FAKE_IDENTITIES=$DEVID$DEVID2$FOOTER" "APPLE_SIGNING_IDENTITY=Developer ID Application: Other Org (ZZZZZ99999)"
identity_case "APPLE_TEAM_ID wins over the identity's" ok \
  "$(printf 'Developer ID Application: Some Body (ABCDE12345)\nQQQQQ11111')" \
  "FAKE_IDENTITIES=$DEVID$FOOTER" APPLE_TEAM_ID=QQQQQ11111
identity_case "identity without a team id" fail "" "APPLE_SIGNING_IDENTITY=Developer ID Application: Nobody"
# release-team-id says UNSIGNED-ADHOC: identity "-" whatever the Keychain or the shell holds
printf '# the unsigned route\nUNSIGNED-ADHOC\n' > "$TMP/adhoc-team-id"
identity_case "release-team-id UNSIGNED-ADHOC: the ad-hoc identity" ok "$(printf -- '-\nUNSIGNED-ADHOC')" \
  "FAKE_IDENTITIES=$DEVID$FOOTER" "RELEASE_TEAM_ID_FILE=$TMP/adhoc-team-id"
identity_case "release-team-id UNSIGNED-ADHOC: exported Apple variables are ignored" ok "$(printf -- '-\nUNSIGNED-ADHOC')" \
  "APPLE_SIGNING_IDENTITY=Developer ID Application: Other Org (ZZZZZ99999)" APPLE_TEAM_ID=ZZZZZ99999 \
  "RELEASE_TEAM_ID_FILE=$TMP/adhoc-team-id"

# --- remap-path-flags.sh and build-mac-local.sh --print-rustflags: no build path in the binaries ---

cat > "$SHIMS/rustc" <<'EOF'
#!/usr/bin/env bash
[ "$*" = "--print sysroot" ] && echo "${FAKE_SYSROOT:-/srv/someone/.rustup/toolchains/1.99.1-aarch64-apple-darwin}"
EOF
chmod +x "$SHIMS/rustc"
out="$TMP/remap.out"
env -i PATH="$SHIMS:/usr/bin:/bin" HOME=/srv/someone bash "$HERE/remap-path-flags.sh" /srv/someone/src/retv-bridge > "$out" 2>&1
want='--remap-path-prefix=/srv/someone=/build/home
--remap-path-prefix=/srv/someone/.rustup=/build/rustup
--remap-path-prefix=/srv/someone/.cargo=/build/cargo
--remap-path-prefix=/srv/someone/.rustup/toolchains/1.99.1-aarch64-apple-darwin=/build/rust-toolchain
--remap-path-prefix=/srv/someone/src/retv-bridge=/build/src'
[ "$(cat "$out")" = "$want" ] && ok "remap flags: home, rustup, cargo, toolchain, workspace, most specific last" || bad "remap flags" "$out"
env -i PATH="$SHIMS:/usr/bin:/bin" HOME=/srv/ci CARGO_HOME=/opt/cargo RUSTUP_HOME=/opt/rustup FAKE_SYSROOT=/opt/rustup/toolchains/x \
  bash "$HERE/remap-path-flags.sh" /work/retv-bridge > "$out" 2>&1
grep -qx -- '--remap-path-prefix=/opt/cargo=/build/cargo' "$out" && grep -qx -- '--remap-path-prefix=/opt/rustup=/build/rustup' "$out" \
  && ok "remap flags: CARGO_HOME and RUSTUP_HOME are followed" || bad "remap flags: CARGO_HOME and RUSTUP_HOME" "$out"
cat > "$SHIMS/cygpath" <<'EOF'
#!/usr/bin/env bash
[ "$1" = -w ] && printf '%s\n' "$2" | sed -e 's#^/c/#C:\\#' -e 's#^/d/#D:\\#' -e 's#/#\\#g'
EOF
chmod +x "$SHIMS/cygpath"
env -i PATH="$SHIMS:/usr/bin:/bin" HOME=/c/u/runneradmin FAKE_SYSROOT='C:\u\runneradmin\.rustup\toolchains\x' \
  bash "$HERE/remap-path-flags.sh" /d/a/retv-bridge/retv-bridge > "$out" 2>&1
rm "$SHIMS/cygpath"
grep -qxF -- '--remap-path-prefix=C:\u\runneradmin=/build/home' "$out" && grep -qxF -- '--remap-path-prefix=D:\a\retv-bridge\retv-bridge=/build/src' "$out" \
  && ok "remap flags: Windows paths under Git Bash" || bad "remap flags: Windows paths under Git Bash" "$out"
env -i PATH="$SHIMS:/usr/bin:/bin" HOME=/srv/someone RUSTFLAGS="-C debuginfo=2" RELEASE_TEAM_ID_FILE="$TMP/no-release-team-id" \
  bash "$HERE/build-mac-local.sh" --print-rustflags > "$out" 2>&1
[ "$(grep -c -- '^--remap-path-prefix=' "$out")" = 5 ] && grep -q -- '^--remap-path-prefix=/srv/someone=/build/home$' "$out" \
  && grep -q -- "=/build/src\$" "$out" && ok "build-mac-local.sh builds with the remap flags" || bad "build-mac-local.sh builds with the remap flags" "$out"

# --- win-sign-hook.mjs: Tauri's signCommand for the two-pass Windows signing ---

if command -v node >/dev/null; then
  HOOK="$HERE/win-sign-hook.mjs"
  W="$TMP/win"
  mkdir -p "$W/target" "$W/nsis-temp" "$W/plugins"
  printf 'MZ app patched' > "$W/target/retv-bridge.exe"
  printf 'MZ uninstaller' > "$W/nsis-temp/ns1234.tmp"
  printf 'MZ plugin' > "$W/plugins/nsDialogs.dll"
  printf 'MZ setup' > "$W/ReTV Bridge_1.0.0_x64-setup.exe"
  hook() { node "$HOOK" "$@" > "$TMP/hook.out" 2>&1; }
  # pass 0: the app and the uninstaller are taken out, nothing else
  if hook capture "$W/inner" "$W/target/retv-bridge.exe" && hook capture "$W/inner" "$W/nsis-temp/ns1234.tmp" \
    && hook capture "$W/inner" "$W/plugins/nsDialogs.dll" && hook capture "$W/inner" "$W/ReTV Bridge_1.0.0_x64-setup.exe"; then
    ok "hook capture: every call succeeds"
  else
    bad "hook capture: every call succeeds" "$TMP/hook.out"
  fi
  [ "$(ls "$W/inner" | sort | tr '\n' ' ')" = "retv-bridge.exe uninstall.exe uninstall.exe.sha256 " ] \
    && ok "  captures exactly the app and the uninstaller" || bad "  captures exactly the app and the uninstaller ($(ls "$W/inner" | tr '\n' ' '))"
  cmp -s "$W/inner/uninstall.exe" "$W/nsis-temp/ns1234.tmp" && ok "  the uninstaller is copied as is" || bad "  the uninstaller is copied as is"
  UNINST_SHA="$(tr -d '[:space:]' < "$W/inner/uninstall.exe.sha256")"
  [ "$UNINST_SHA" = "$(shasum -a 256 "$W/nsis-temp/ns1234.tmp" | cut -d' ' -f1)" ] \
    && ok "  records the uninstaller's SHA-256" || bad "  records the uninstaller's SHA-256"
  hook capture "$W/inner" "$W/nsis-temp/ns1234.tmp" && bad "hook capture: a second uninstaller is refused" \
    || ok "hook capture: a second uninstaller is refused"
  # Windows paths: the name decides, whatever the separators
  hook capture "$W/inner2" 'C:\a\target\release\retv-bridge.exe' && bad "hook: a missing file fails" "$TMP/hook.out" || ok "hook: a missing file fails"

  # pass 2: SignPath's output in signed/, the same build again in target/ and nsis-temp/
  mkdir -p "$W/signed"
  printf 'MZ app patched+sig' > "$W/signed/retv-bridge.exe"
  printf 'MZ uninstaller+sig' > "$W/signed/uninstall.exe"
  cp "$W/signed/retv-bridge.exe" "$W/target/retv-bridge.exe"
  if hook swap "$W/signed" "$UNINST_SHA" "$W/target/retv-bridge.exe" && hook swap "$W/signed" "$UNINST_SHA" "$W/nsis-temp/ns1234.tmp" \
    && hook swap "$W/signed" "$UNINST_SHA" "$W/plugins/nsDialogs.dll" && hook swap "$W/signed" "$UNINST_SHA" "$W/ReTV Bridge_1.0.0_x64-setup.exe"; then
    ok "hook swap: every call succeeds"
  else
    bad "hook swap: every call succeeds" "$TMP/hook.out"
  fi
  cmp -s "$W/nsis-temp/ns1234.tmp" "$W/signed/uninstall.exe" && ok "  the signed uninstaller replaces the built one" \
    || bad "  the signed uninstaller replaces the built one"
  [ "$(cat "$W/plugins/nsDialogs.dll")" = "MZ plugin" ] && [ "$(cat "$W/ReTV Bridge_1.0.0_x64-setup.exe")" = "MZ setup" ] \
    && ok "  plugins and the installer are left alone" || bad "  plugins and the installer are left alone"
  printf 'MZ uninstaller, another build' > "$W/nsis-temp/other.tmp"
  hook swap "$W/signed" "$UNINST_SHA" "$W/nsis-temp/other.tmp" && bad "hook swap: refuses an uninstaller other than the signed one" \
    || { grep -q "not the one signed in pass 1" "$TMP/hook.out" && ok "hook swap: refuses an uninstaller other than the signed one" \
      || bad "hook swap: refuses an uninstaller other than the signed one" "$TMP/hook.out"; }
  [ "$(cat "$W/nsis-temp/other.tmp")" = "MZ uninstaller, another build" ] && ok "  and leaves it untouched" || bad "  and leaves it untouched"
  printf 'MZ app unsigned' > "$W/target/retv-bridge.exe"
  hook swap "$W/signed" "$UNINST_SHA" "$W/target/retv-bridge.exe" && bad "hook swap: refuses an app that is not the signed one" \
    || ok "hook swap: refuses an app that is not the signed one"
  hook swap "$W/signed" "not-a-hash" "$W/nsis-temp/ns1234.tmp" && bad "hook swap: refuses a bad hash argument" \
    || ok "hook swap: refuses a bad hash argument"
  hook sign "$W/target/retv-bridge.exe" && bad "hook: an unknown mode is refused" || ok "hook: an unknown mode is refused"
else
  echo "skip win-sign-hook.mjs (no node on PATH)"
fi

echo
echo "$GOOD passed, $FAILED failed"
[ "$FAILED" -eq 0 ]
