#!/usr/bin/env bash
# Prints rustc --remap-path-prefix flags, one per line, so that no path of the build machine ends up in a binary
# (panic locations, file!()): the home folder, rustup, the cargo home (registry and git sources), the Rust toolchain and
# the workspace. rustc applies the last flag that matches, so the more specific ones come last.
#
# Usage: scripts/remap-path-flags.sh [workspace]   (default: the bridge root)
# Used by scripts/build-mac-local.sh (as CARGO_ENCODED_RUSTFLAGS) and by the release workflow (as RUSTFLAGS).
# Under Git Bash on Windows the paths are printed the way rustc sees them (cygpath -w).
set -euo pipefail

ROOT="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
HOME_DIR="${HOME:?HOME is not set}"
native() { if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else printf '%s\n' "$1"; fi; }
emit() { printf -- '--remap-path-prefix=%s=%s\n' "$(native "$1")" "$2"; }

SYSROOT="$(rustc --print sysroot)"
[ -n "$SYSROOT" ] || { echo "remap-path-flags: rustc printed no sysroot" >&2; exit 1; }

emit "$HOME_DIR" /build/home
emit "${RUSTUP_HOME:-$HOME_DIR/.rustup}" /build/rustup
emit "${CARGO_HOME:-$HOME_DIR/.cargo}" /build/cargo
emit "$SYSROOT" /build/rust-toolchain
emit "$ROOT" /build/src
