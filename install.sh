#!/bin/sh
set -e

# Monocle CLI installer. Downloads a prebuilt standalone binary from GitHub
# Releases — no Node, no npm. Usage:
#   curl -fsSL https://raw.githubusercontent.com/warmblood-kr/monocle-cli/main/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- v0.5.0       # pin a version

REPO="warmblood-kr/monocle-cli"
BINARY="monocle"

resolve_install_dir() {
  if [ -n "$MONOCLE_INSTALL_DIR" ]; then
    echo "$MONOCLE_INSTALL_DIR"
    return
  fi
  USER_BIN="${HOME}/.local/bin"
  if [ -d "$USER_BIN" ] && [ -w "$USER_BIN" ]; then
    echo "$USER_BIN"
    return
  fi
  if [ -w "$HOME" ]; then
    echo "$USER_BIN"
    return
  fi
  echo "/usr/local/bin"
}

get_latest_version() {
  curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"//;s/".*//'
}

detect_platform() {
  OS="$(uname -s)"
  ARCH="$(uname -m)"
  case "$OS" in
    Darwin)
      case "$ARCH" in
        arm64|aarch64) echo "macos-arm64" ;;
        x86_64|amd64) echo "unsupported: macOS Intel (x86_64) is not shipped — use an Apple Silicon Mac or build from source" && exit 1 ;;
        *) echo "unsupported: macOS $ARCH" && exit 1 ;;
      esac
      ;;
    Linux)
      case "$ARCH" in
        x86_64|amd64) echo "linux-x64" ;;
        arm64|aarch64) echo "linux-arm64" ;;
        *) echo "unsupported: Linux $ARCH" && exit 1 ;;
      esac
      ;;
    MINGW*|MSYS*|CYGWIN*) echo "windows-x64" ;;
    *) echo "unsupported: $OS" && exit 1 ;;
  esac
}

# Verify $1 (a downloaded file path) against its entry for $2 (the asset
# filename as it appears in SHA256SUMS) in $3 (the release's SHA256SUMS,
# already downloaded to a local path). A missing sums file or missing entry
# is a soft warning (old releases predate this — must still install); an
# actual hash mismatch is a hard error, since that's the real security check.
verify_checksum() {
  FILE="$1"
  ASSET_NAME="$2"
  SUMS_FILE="$3"

  if [ ! -f "$SUMS_FILE" ]; then
    echo "Warning: SHA256SUMS not available, skipping checksum verification." >&2
    return 0
  fi

  EXPECTED="$(grep -F "$ASSET_NAME" "$SUMS_FILE" | awk '{print $1}' | head -1)"
  if [ -z "$EXPECTED" ]; then
    echo "Warning: no checksum entry for ${ASSET_NAME}, skipping checksum verification." >&2
    return 0
  fi

  if command -v sha256sum >/dev/null 2>&1; then
    ACTUAL="$(sha256sum "$FILE" | awk '{print $1}')"
  elif command -v shasum >/dev/null 2>&1; then
    ACTUAL="$(shasum -a 256 "$FILE" | awk '{print $1}')"
  else
    echo "Warning: no sha256sum/shasum found, skipping checksum verification." >&2
    return 0
  fi

  if [ "$EXPECTED" != "$ACTUAL" ]; then
    echo "Error: checksum verification failed for ${ASSET_NAME}" >&2
    echo "  expected: ${EXPECTED}" >&2
    echo "  actual:   ${ACTUAL}" >&2
    exit 1
  fi
  echo "Checksum verified: ${ASSET_NAME}"
}

in_path() {
  case ":$PATH:" in
    *":$1:"*) return 0 ;;
    *) return 1 ;;
  esac
}

main() {
  VERSION="${1:-$(get_latest_version)}"
  if [ -z "$VERSION" ]; then
    echo "Error: could not determine latest version"
    exit 1
  fi

  PLATFORM="$(detect_platform)"
  INSTALL_DIR="$(resolve_install_dir)"

  echo "Installing monocle ${VERSION} for ${PLATFORM}..."
  echo "Target: ${INSTALL_DIR}"
  echo ""

  mkdir -p "$INSTALL_DIR" 2>/dev/null || true

  SUMS_URL="https://github.com/${REPO}/releases/download/${VERSION}/SHA256SUMS"

  case "$PLATFORM" in
    *windows*)
      ASSET="monocle-${PLATFORM}.zip"
      URL="https://github.com/${REPO}/releases/download/${VERSION}/${ASSET}"
      TMPDIR="$(mktemp -d)"
      curl -fsSL "$URL" -o "${TMPDIR}/${ASSET}"
      curl -fsSL "$SUMS_URL" -o "${TMPDIR}/SHA256SUMS" 2>/dev/null || true
      verify_checksum "${TMPDIR}/${ASSET}" "${ASSET}" "${TMPDIR}/SHA256SUMS"
      unzip -o "${TMPDIR}/${ASSET}" -d "${TMPDIR}" > /dev/null
      mv "${TMPDIR}/${BINARY}.exe" "${INSTALL_DIR}/${BINARY}.exe"
      rm -rf "$TMPDIR"
      echo "Installed: ${INSTALL_DIR}/${BINARY}.exe"
      ;;
    *)
      ASSET="monocle-${PLATFORM}.tar.gz"
      URL="https://github.com/${REPO}/releases/download/${VERSION}/${ASSET}"
      TMPDIR="$(mktemp -d)"
      curl -fsSL "$URL" -o "${TMPDIR}/${ASSET}"
      curl -fsSL "$SUMS_URL" -o "${TMPDIR}/SHA256SUMS" 2>/dev/null || true
      verify_checksum "${TMPDIR}/${ASSET}" "${ASSET}" "${TMPDIR}/SHA256SUMS"
      tar xzf "${TMPDIR}/${ASSET}" -C "${TMPDIR}"
      chmod +x "${TMPDIR}/${BINARY}"
      if [ -w "$INSTALL_DIR" ]; then
        mv "${TMPDIR}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
      else
        echo "${INSTALL_DIR} is not writable, falling back to sudo."
        sudo mv "${TMPDIR}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
      fi
      rm -rf "$TMPDIR"
      echo "Installed: ${INSTALL_DIR}/${BINARY}"
      ;;
  esac

  echo ""
  if ! in_path "$INSTALL_DIR"; then
    echo "WARNING: ${INSTALL_DIR} is not in your PATH."
    echo "Add this to your shell config (~/.zshrc, ~/.bashrc, etc.):"
    echo ""
    echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
    echo ""
  fi

  echo "Run 'monocle login' to get started."
}

main "$@"
