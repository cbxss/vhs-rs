#!/bin/sh
# vhs-rs installer
#
#   curl -fsSL https://raw.githubusercontent.com/cbxss/vhs-rs/main/install.sh | sh
#
# Downloads the latest release binary for your platform and installs it to
# ~/.local/bin (override with VHS_RS_INSTALL_DIR). Set VHS_RS_VERSION (e.g.
# v0.1.2) to pin a release and skip the GitHub API lookup entirely.
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

# Resolve the release tag. Fetch to a variable BEFORE grepping: in a
# `curl | grep -m1 | cut` pipeline the exit status is cut's, so a failed API
# call (offline, or the unauthenticated 60/hr rate limit) sailed past the
# `|| die`, left $tag empty, and produced a baffling `download//` 404 — and
# grep's early exit made curl print a spurious "(23) Failure writing output"
# even on success.
if [ -n "${VHS_RS_VERSION:-}" ]; then
  tag="$VHS_RS_VERSION"
else
  api="https://api.github.com/repos/$REPO/releases/latest"
  json=$(curl -fsSL "$api") ||
    die "could not query $api — offline, or rate-limited by the GitHub API?
  Retry later, or pin a release: VHS_RS_VERSION=v0.1.2 sh install.sh"
  tag=$(printf '%s\n' "$json" | grep -m1 '"tag_name"' | cut -d'"' -f4)
  [ -n "$tag" ] || die "could not parse a release tag out of $api"
fi

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
