#!/bin/sh
# lsw installer — downloads the latest release binary to ~/.local/bin
#
#   curl -fsSL https://raw.githubusercontent.com/apilot/lsw/main/install.sh | sh
#
# Override the destination:  curl ... | LSW_PREFIX=/usr/local/bin sh
set -eu

REPO="apilot/lsw"

case "$(uname -m)" in
  x86_64) ARCH=x86_64 ;;
  *)
    echo "lsw installer: no prebuilt binary for $(uname -m)." >&2
    echo "Install from source instead:" >&2
    echo "  cargo install --git https://github.com/$REPO" >&2
    exit 1
    ;;
esac

command -v curl >/dev/null || { echo "curl is required" >&2; exit 1; }

LATEST=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" |
  sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p')
[ -n "$LATEST" ] || { echo "failed to resolve the latest release" >&2; exit 1; }

ASSET="lsw-$LATEST-$ARCH-linux.tar.gz"
BASE="https://github.com/$REPO/releases/download/$LATEST"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

echo "lsw installer: downloading $ASSET"
curl -fsSL "$BASE/$ASSET" -o "$TMP/$ASSET"
curl -fsSL "$BASE/$ASSET.sha256" -o "$TMP/$ASSET.sha256"
(cd "$TMP" && sha256sum -c "$ASSET.sha256" >/dev/null)

tar xzf "$TMP/$ASSET" -C "$TMP"

DEST="${LSW_PREFIX:-$HOME/.local/bin}"
mkdir -p "$DEST"
install -m 755 "$TMP/lsw" "$DEST/lsw"

echo "installed: $DEST/lsw"
"$DEST/lsw" --version 2>/dev/null || true
case ":$PATH:" in
  *":$DEST:"*) ;;
  *) echo "note: add $DEST to your PATH to run 'lsw'" ;;
esac
