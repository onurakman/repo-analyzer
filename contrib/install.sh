#!/usr/bin/env sh
# Install script for repo-analyzer.
#
# Usage:
#   curl -sfL https://raw.githubusercontent.com/onurakman/repo-analyzer/master/contrib/install.sh | sh -s -- -b /usr/local/bin
#   curl -sfL https://raw.githubusercontent.com/onurakman/repo-analyzer/master/contrib/install.sh | sh -s -- -b /usr/local/bin v0.1.3
#
# Flags:
#   -b DIR   Install the binary into DIR (default: /usr/local/bin).
#            DIR must exist; create it first if needed.
#   -d       Print debug information.
#   -h       Print this help.
#
# Positional:
#   VERSION  Git tag to install (e.g. v0.1.3). Defaults to the latest release.
#            Accepts both "v0.1.3" and "0.1.3".
#
# The script downloads the matching release asset from GitHub, verifies the
# file is non-empty, sets the executable bit, and moves it to DIR. No sudo is
# invoked — re-run the command with sudo if DIR is not user-writable.

set -eu

REPO_OWNER="onurakman"
REPO_NAME="repo-analyzer"
BIN_NAME="repo-analyzer"
BIN_DIR="/usr/local/bin"
DEBUG=0
VERSION=""

usage() {
  sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-0}"
}

log()   { printf '[install] %s\n' "$*" >&2; }
debug() { [ "$DEBUG" -eq 1 ] && printf '[debug] %s\n' "$*" >&2 || :; }
fail()  { printf '[install] error: %s\n' "$*" >&2; exit 1; }

# --- arg parsing ---------------------------------------------------------
while [ $# -gt 0 ]; do
  case "$1" in
    -b) shift; [ $# -gt 0 ] || fail "-b requires a directory"; BIN_DIR="$1"; shift;;
    -d) DEBUG=1; shift;;
    -h|--help) usage 0;;
    -*) fail "unknown flag: $1";;
    *)  VERSION="$1"; shift;;
  esac
done

# --- prerequisites -------------------------------------------------------
have() { command -v "$1" >/dev/null 2>&1; }
have curl || fail "curl is required"
have uname || fail "uname is required"

DOWNLOADER=""
if have curl; then DOWNLOADER="curl -fsSL"
elif have wget; then DOWNLOADER="wget -qO-"
else fail "need curl or wget"; fi

# --- detect OS / arch ----------------------------------------------------
uname_s=$(uname -s)
uname_m=$(uname -m)

case "$uname_s" in
  Linux)  os="linux";;
  Darwin) os="macos";;
  *)      fail "unsupported OS: $uname_s (only linux/macos have a shell installer; for Windows download the .exe manually)";;
esac

case "$uname_m" in
  x86_64|amd64)       arch="amd64";;
  aarch64|arm64)      arch="arm64";;
  *)                  fail "unsupported arch: $uname_m";;
esac

ASSET="${BIN_NAME}-${os}-${arch}"
debug "os=$os arch=$arch asset=$ASSET"

# --- resolve version -----------------------------------------------------
normalize_version() {
  # Accept both v0.1.3 and 0.1.3; emit v0.1.3.
  case "$1" in
    v*) printf '%s' "$1";;
    *)  printf 'v%s' "$1";;
  esac
}

if [ -z "$VERSION" ]; then
  log "resolving latest release..."
  # Follow redirect on /releases/latest to get the final tag without hitting
  # the rate-limited API. `curl -sI` returns the Location header.
  latest_url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
    "https://github.com/${REPO_OWNER}/${REPO_NAME}/releases/latest")
  VERSION="${latest_url##*/}"
  [ -n "$VERSION" ] || fail "could not resolve latest release"
fi
VERSION=$(normalize_version "$VERSION")
debug "version=$VERSION"

# --- download ------------------------------------------------------------
URL="https://github.com/${REPO_OWNER}/${REPO_NAME}/releases/download/${VERSION}/${ASSET}"
TMP_DIR=$(mktemp -d 2>/dev/null || mktemp -d -t repo-analyzer-install)
trap 'rm -rf "$TMP_DIR"' EXIT INT TERM
TMP_BIN="${TMP_DIR}/${BIN_NAME}"

log "downloading $URL"
if ! curl -fsSL -o "$TMP_BIN" "$URL"; then
  fail "download failed (check that $VERSION has an asset named $ASSET at https://github.com/${REPO_OWNER}/${REPO_NAME}/releases)"
fi
[ -s "$TMP_BIN" ] || fail "downloaded file is empty"
chmod +x "$TMP_BIN"

# --- install -------------------------------------------------------------
[ -d "$BIN_DIR" ] || fail "install dir does not exist: $BIN_DIR"

DEST="${BIN_DIR}/${BIN_NAME}"
if ! mv "$TMP_BIN" "$DEST" 2>/dev/null; then
  fail "could not move binary to $DEST (try re-running with sudo)"
fi

log "installed $DEST"
"$DEST" --version 2>/dev/null || log "binary in place; run '$DEST --version' to verify"
