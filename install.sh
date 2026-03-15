#!/usr/bin/env sh
# netface installer — detects OS/arch and installs the latest release
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/tristanMatthias/netface/main/install.sh | sh
#   # or pin a version:
#   NETFACE_VERSION=v0.2.0 sh install.sh

set -e

REPO="tristanMatthias/netface"
BIN="netface"
INSTALL_DIR="${NETFACE_INSTALL_DIR:-/usr/local/bin}"

# ── Detect OS ────────────────────────────────────────────────────────────────
OS="$(uname -s)"
case "$OS" in
  Linux)  OS="linux" ;;
  Darwin) OS="macos" ;;
  *)
    echo "Unsupported OS: $OS" >&2
    exit 1
    ;;
esac

# ── Detect architecture ──────────────────────────────────────────────────────
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64 | amd64)          ARCH="x86_64" ;;
  aarch64 | arm64 | armv8*) ARCH="aarch64" ;;
  *)
    echo "Unsupported architecture: $ARCH" >&2
    exit 1
    ;;
esac

# ── Map to release target triple ─────────────────────────────────────────────
if [ "$OS" = "macos" ]; then
  TARGET="${ARCH}-apple-darwin"
elif [ "$OS" = "linux" ]; then
  TARGET="${ARCH}-unknown-linux-gnu"
fi

# ── Resolve version ──────────────────────────────────────────────────────────
if [ -z "$NETFACE_VERSION" ]; then
  echo "Fetching latest release..."
  NETFACE_VERSION="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep '"tag_name"' | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')"
fi

if [ -z "$NETFACE_VERSION" ]; then
  echo "Could not determine latest version. Set NETFACE_VERSION explicitly." >&2
  exit 1
fi

echo "Installing netface ${NETFACE_VERSION} (${TARGET})..."

# ── Download ─────────────────────────────────────────────────────────────────
ARCHIVE="${BIN}-${TARGET}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${NETFACE_VERSION}/${ARCHIVE}"
TMP="$(mktemp -d)"

echo "Downloading ${URL}..."
curl -fsSL --progress-bar "$URL" -o "${TMP}/${ARCHIVE}"

tar -xzf "${TMP}/${ARCHIVE}" -C "$TMP"
chmod +x "${TMP}/${BIN}"

# ── Install ───────────────────────────────────────────────────────────────────
# Try /usr/local/bin first; fall back to ~/.local/bin if no write permission
if [ -w "$INSTALL_DIR" ] || sudo -n true 2>/dev/null; then
  if [ ! -w "$INSTALL_DIR" ]; then
    sudo mv "${TMP}/${BIN}" "${INSTALL_DIR}/${BIN}"
    echo "Installed to ${INSTALL_DIR}/${BIN} (used sudo)"
  else
    mv "${TMP}/${BIN}" "${INSTALL_DIR}/${BIN}"
    echo "Installed to ${INSTALL_DIR}/${BIN}"
  fi
else
  FALLBACK="$HOME/.local/bin"
  mkdir -p "$FALLBACK"
  mv "${TMP}/${BIN}" "${FALLBACK}/${BIN}"
  echo "Installed to ${FALLBACK}/${BIN}"
  echo ""
  echo "Make sure ${FALLBACK} is in your PATH:"
  echo "  export PATH=\"\$PATH:${FALLBACK}\""
fi

rm -rf "$TMP"
echo ""
echo "Done! Run: netface --help"
