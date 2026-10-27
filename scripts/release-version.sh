#!/usr/bin/env bash
# Checks a release tag and prints its version.
#
# Usage: scripts/release-version.sh <tag> [bridge-root]
#   <tag>  bridge-v<semver> (a refs/tags/ prefix is accepted; no hyphen inside a pre-release)
# Fails unless the version equals app/tauri.conf.json "version", the Cargo workspace version (Cargo.toml and
# Cargo.lock), plugins.updater.pubkey is a real key, and release-notes/<version>.json follows release-notes/README.md.
#
#        scripts/release-version.sh --set <version> [bridge-root]
# Writes <version> into all three places that hold it: app/tauri.conf.json "version" (Tauri, the updater), the
# [workspace.package] version of Cargo.toml (CARGO_PKG_VERSION: hello, the User-Agent) and the workspace crates in
# Cargo.lock (so --locked builds still pass). The check above refuses a tag while any of them differs.
set -euo pipefail

# No hyphen inside the pre-release: the manifest (retv-bridge-manifest.ts) accepts only -[0-9A-Za-z.]+.
SEMVER='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z]+(\.[0-9A-Za-z]+)*)?$'

if [ "${1:-}" = --set ]; then
  export NEW="${2:?usage: $0 --set <version> [bridge-root]}"
  ROOT="${3:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
  [[ "$NEW" =~ $SEMVER ]] || { echo "$NEW is not a semver version" >&2; exit 1; }
  # tauri.conf.json: the top-level "version" only (two-space indent, as the file is written)
  perl -0pi -e 's/^(  "version":\s*")[^"]*(")/${1}$ENV{NEW}${2}/m or die "no top-level version in $ARGV\n"' "$ROOT/app/tauri.conf.json"
  perl -0pi -e 's/(^\[workspace\.package\][^\[]*?^version\s*=\s*")[^"]*(")/${1}$ENV{NEW}${2}/ms or die "no [workspace.package] version in $ARGV\n"' "$ROOT/Cargo.toml"
  # Cargo.lock: the packages without a source are this workspace's own crates
  perl -00 -pi -e 's/^(version = ")[^"]*(")/${1}$ENV{NEW}${2}/m if /^\[\[package\]\]/m && !/^source = /m' "$ROOT/Cargo.lock"
  echo "$NEW"
  exit 0
fi

TAG="${1:?usage: $0 <tag> [bridge-root]}"
ROOT="${2:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
TAG="${TAG#refs/tags/}"

case "$TAG" in
  bridge-v*) VERSION="${TAG#bridge-v}" ;;
  *) echo "tag $TAG does not start with bridge-v" >&2; exit 1 ;;
esac
if ! [[ "$VERSION" =~ $SEMVER ]]; then
  echo "tag $TAG: $VERSION is not a semver version" >&2
  exit 1
fi

CONF="$ROOT/app/tauri.conf.json"
NOTES="$ROOT/release-notes/$VERSION.json"

# perl + JSON::PP (core Perl) is on every runner and on macOS; no jq needed.
CONF_VERSION="$(perl -MJSON::PP -e '
  local $/; open(my $h, "<", $ARGV[0]) or die "cannot read $ARGV[0]\n";
  my $c = JSON::PP->new->decode(<$h>);
  print defined $c->{version} ? $c->{version} : "";
' "$CONF")"
if [ "$CONF_VERSION" != "$VERSION" ]; then
  echo "tag $TAG says $VERSION, app/tauri.conf.json says ${CONF_VERSION:-nothing}" >&2
  exit 1
fi

# hello, the User-Agent and the updater's own version read CARGO_PKG_VERSION: the Cargo workspace must say the same.
CARGO_VERSION="$(perl -0ne 'print $1 if /^\[workspace\.package\][^\[]*?^version\s*=\s*"([^"]*)"/ms' "$ROOT/Cargo.toml")"
if [ "$CARGO_VERSION" != "$VERSION" ]; then
  echo "tag $TAG says $VERSION, Cargo.toml [workspace.package] says ${CARGO_VERSION:-nothing}: run scripts/release-version.sh --set $VERSION" >&2
  exit 1
fi
LOCK_OFF="$(V="$VERSION" perl -00 -ne 'if (/^\[\[package\]\]/m && !/^source = /m && /^name = "([^"]*)"/m) { my $n = $1; my ($v) = /^version = "([^"]*)"/m; print "$n $v\n" if $v ne $ENV{V} }' "$ROOT/Cargo.lock")"
if [ -n "$LOCK_OFF" ]; then
  echo "Cargo.lock has workspace crates at another version than $VERSION ($(printf '%s' "$LOCK_OFF" | tr '\n' ',')): run scripts/release-version.sh --set $VERSION" >&2
  exit 1
fi

# A build with the placeholder (or no) updater pubkey can never update itself.
PUBKEY="$(perl -MJSON::PP -e '
  local $/; open(my $h, "<", $ARGV[0]) or die "cannot read $ARGV[0]\n";
  my $c = JSON::PP->new->decode(<$h>);
  my $u = ref $c->{plugins} eq "HASH" && ref $c->{plugins}{updater} eq "HASH" ? $c->{plugins}{updater} : {};
  print defined $u->{pubkey} && !ref $u->{pubkey} ? $u->{pubkey} : "";
' "$CONF")"
case "$(printf '%s' "$PUBKEY" | tr -d '[:space:]')" in
  ""|REPLACE_WITH_THE_RETV_BRIDGE_UPDATER_PUBLIC_KEY)
    echo "app/tauri.conf.json: plugins.updater.pubkey is not the real updater public key; a release built like this never updates." >&2
    echo "app/tauri.conf.json: в plugins.updater.pubkey нет настоящего публичного ключа обновлений; такая сборка никогда не обновится." >&2
    echo "Put the public key of the owner's updater key there (docs/bridge/owner-setup.md §4) and tag again." >&2
    exit 1 ;;
esac

if [ ! -f "$NOTES" ]; then
  echo "missing release-notes/$VERSION.json" >&2
  exit 1
fi
# The same rules as the manifest the staging script writes (the web release scripts, retv-home-manifest.ts
# marketNotes): en and ru required, uk optional, each 1-8 non-blank lines of at most 200 characters, and no Cyrillic
# in en (it ships on retv.app). Checked here so a tag fails in minutes, not after a full signed build.
perl -MJSON::PP -e '
  local $/; open(my $h, "<:raw", $ARGV[0]) or die "cannot read $ARGV[0]\n";
  my $n = eval { JSON::PP->new->utf8->decode(<$h>) };
  my $f = "release-notes/$ARGV[1].json";
  die "$f is not JSON\n" unless ref $n eq "HASH";
  for my $lang (qw(en ru uk)) {
    next if $lang eq "uk" && !exists $n->{uk};
    my $l = $n->{$lang};
    die "$f: \"$lang\" must be a list of 1-8 strings of at most 200 characters, none blank (see release-notes/README.md)\n"
      unless ref $l eq "ARRAY" && @$l >= 1 && @$l <= 8
        && !grep { ref $_ || !defined $_ || $_ !~ /\S/ || length($_) > 200 } @$l;
  }
  die "$f: \"en\" must not contain Cyrillic (it ships on retv.app)\n"
    if grep { /[\x{0400}-\x{052F}\x{1C80}-\x{1C8F}\x{2DE0}-\x{2DFF}\x{A640}-\x{A69F}]/ } @{$n->{en}};
' "$NOTES" "$VERSION"

echo "$VERSION"
