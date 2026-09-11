#!/bin/sh
# intraweb installer -- your neighborhood web
#
# Fetches the release binary for this machine and puts it on your PATH.
# Needs the internet exactly once; intraweb itself never does.
set -eu

REPO="2008wbbv/intraweb"
BIN="intraweb"
PREFIX="${PREFIX:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

os=$(uname -s)
arch=$(uname -m)

case "$os" in
  Linux)  os_tag="unknown-linux-musl" ;;
  Darwin) os_tag="apple-darwin" ;;
  *) die "unsupported operating system: $os" ;;
esac

case "$arch" in
  x86_64|amd64) arch_tag="x86_64" ;;
  aarch64|arm64) arch_tag="aarch64" ;;
  armv6l|armv7l) die "32-bit ARM is not a supported target; see the README" ;;
  *) die "unsupported architecture: $arch" ;;
esac

target="${arch_tag}-${os_tag}"
url="https://github.com/${REPO}/releases/latest/download/${BIN}-${target}"

command -v curl >/dev/null 2>&1 || die "curl is required"

say "Fetching ${BIN} for ${target}..."
mkdir -p "$PREFIX"
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT

curl -fsSL "$url" -o "$tmp" || die "no release build for ${target} yet -- build from source with: cargo build --release"
chmod +x "$tmp"
mv "$tmp" "$PREFIX/$BIN"
trap - EXIT

say "Installed to $PREFIX/$BIN"

case ":$PATH:" in
  *":$PREFIX:"*) ;;
  *) say ""; say "Add it to your PATH:"; say "  export PATH=\"$PREFIX:\$PATH\"" ;;
esac

say ""
say "Get started:  $BIN up"
say "Host a hub:   $BIN up --hub"
