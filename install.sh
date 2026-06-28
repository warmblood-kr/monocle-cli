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
        x86_64|amd64) echo "macos-x64" ;;
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

  case "$PLATFORM" in
    *windows*)
      ASSET="monocle-${PLATFORM}.zip"
      URL="https://github.com/${REPO}/releases/download/${VERSION}/${ASSET}"
      TMPDIR="$(mktemp -d)"
      curl -fsSL "$URL" -o "${TMPDIR}/${ASSET}"
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
