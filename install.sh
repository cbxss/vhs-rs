#!/bin/sh
# vhs-rs installer
#
#   curl -fsSL https://raw.githubusercontent.com/cbxss/vhs-rs/main/install.sh | sh
#
# Downloads the latest release binary for your platform and installs it to
# ~/.local/bin (override with VHS_RS_INSTALL_DIR). While the repository is
# private, an authenticated `gh` CLI (or a GITHUB_TOKEN env var) is required;
# once public, plain curl works.
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

auth_header=""
if [ -n "${GITHUB_TOKEN:-}" ]; then
  auth_header="Authorization: Bearer $GITHUB_TOKEN"
fi

# Resolve the latest release tag.
api="https://api.github.com/repos/$REPO/releases/latest"
if [ -n "$auth_header" ]; then
  tag=$(curl -fsSL -H "$auth_header" "$api" 2>/dev/null | grep -m1 '"tag_name"' | cut -d'"' -f4) || tag=""
else
  tag=$(curl -fsSL "$api" 2>/dev/null | grep -m1 '"tag_name"' | cut -d'"' -f4) || tag=""
fi

asset="vhs-rs-$target.tar.gz"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

fetched=""
if [ -n "$tag" ]; then
  url="https://github.com/$REPO/releases/download/$tag/$asset"
  say "downloading $asset ($tag)"
  if [ -n "$auth_header" ]; then
    # Asset downloads on private repos need the API endpoint; try gh first.
    curl -fsSL -H "$auth_header" -H "Accept: application/octet-stream" "$url" -o "$tmp/$asset" 2>/dev/null && fetched=1 || true
  else
    curl -fsSL "$url" -o "$tmp/$asset" 2>/dev/null && fetched=1 || true
  fi
fi

# Fallback: authenticated gh CLI (works on the private repo).
if [ -z "$fetched" ]; then
  if command -v gh >/dev/null 2>&1; then
    say "direct download unavailable; trying gh CLI"
    gh release download --repo "$REPO" --pattern "$asset" --dir "$tmp" ||
      die "gh release download failed — is a release published and are you authenticated?"
  else
    die "could not download release asset (private repo?) — install the gh CLI and run: gh auth login, then re-run"
  fi
fi

tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$INSTALL_DIR"
install -m 755 "$tmp/vhs-rs" "$INSTALL_DIR/vhs-rs"

say "installed $("$INSTALL_DIR/vhs-rs" --version) to $INSTALL_DIR/vhs-rs"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) say "note: $INSTALL_DIR is not on your PATH" ;;
esac
