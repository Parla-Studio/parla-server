#!/bin/sh
# Makes sure a Rust toolchain and a C compiler (needed for the bundled SQLite) are available.
# If cargo is missing, rustup installs a minimal stable toolchain into ~/.cargo once.
#
# Usage: scripts/setup-rust.sh > .build-env && . ./.build-env   -> puts cargo on PATH
set -e
log() { echo "setup-rust: $*" >&2; }

CARGO_BIN="${CARGO_HOME:-$HOME/.cargo}/bin"
PATH="$CARGO_BIN:$PATH"

if ! command -v cc >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1 && ! command -v clang >/dev/null 2>&1; then
  log "a C compiler is required to build the bundled SQLite (e.g. apt install build-essential)"
  exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
  log "installing Rust (stable, minimal profile) into ${CARGO_HOME:-$HOME/.cargo}"
  TMP=$(mktemp -d)
  trap 'rm -rf "$TMP"' EXIT
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL -o "$TMP/rustup-init.sh" https://sh.rustup.rs
  else
    wget -q -O "$TMP/rustup-init.sh" https://sh.rustup.rs
  fi
  sh "$TMP/rustup-init.sh" -y --profile minimal --default-toolchain stable --no-modify-path >&2
fi

log "using $(cargo --version)"
echo "export PATH=\"$CARGO_BIN:\$PATH\""
