#!/bin/sh
# vhs-rs installer
#
#   curl -fsSL https://raw.githubusercontent.com/cbxss/vhs-rs/main/install.sh | sh
#
# Downloads the latest release binary for your platform and installs it to
# ~/.local/bin (override with VHS_RS_INSTALL_DIR).
set -eu

REPO="cbxss/vhs-rs"
INSTALL_DIR="${VHS_RS_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf 'vhs-rs install: %s\n' "$1" >&2; }
die() { say "error: $1"; exit 1; }

os=$(uname -s)
arch=$(uname -m)
case "$os/$arch" in
  Linux/x86_64)  target="x86_64-unknown-linux-musl" ;;
  Darwin/arm64)  target="aarch64-apple-darwin" ;;
  *) die "no prebuilt binary for $os/$arch yet — build from source: cargo build --release" ;;
esac

tag=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | grep -m1 '"tag_name"' | cut -d'"' -f4) ||
  die "could not resolve the latest release (is github.com reachable?)"

asset="vhs-rs-$target.tar.gz"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

say "downloading $asset ($tag)"
curl -fsSL "https://github.com/$REPO/releases/download/$tag/$asset" -o "$tmp/$asset" ||
  die "download failed: https://github.com/$REPO/releases/download/$tag/$asset"

tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$INSTALL_DIR"
install -m 755 "$tmp/vhs-rs-$target/vhs-rs" "$INSTALL_DIR/vhs-rs"

say "installed $("$INSTALL_DIR/vhs-rs" --version) to $INSTALL_DIR/vhs-rs"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) say "note: $INSTALL_DIR is not on your PATH" ;;
esac
