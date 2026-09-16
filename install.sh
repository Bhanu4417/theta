#!/bin/sh
# Theta installer.
#
#   curl -fsSL https://raw.githubusercontent.com/Bhanu4417/theta/main/install.sh | sh
#
# Downloads the release for this platform, verifies its SHA-256, and installs
# the binary. Nothing else is touched.
#
# Environment:
#   THETA_VERSION      version to install, e.g. 0.1.0 (default: latest release)
#   THETA_INSTALL_DIR  destination directory (default: ~/.local/bin)
#   THETA_BASE_URL     release download base (used by the tests)
#
# Flags:
#   --version <v>   install a specific version
#   --dir <path>    install somewhere else
#   --dry-run       report what would happen, download nothing
#   --help          this text

set -eu

REPO="Bhanu4417/theta"
BIN="theta"
BIN_NAME="Theta"

VERSION="${THETA_VERSION:-}"
INSTALL_DIR="${THETA_INSTALL_DIR:-}"
DRY_RUN=0

say()  { printf '%s\n' "$*"; }
warn() { printf '%s\n' "$*" >&2; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() {
    # Embedded rather than read from $0: under `curl | sh` there is no script
    # file to read, so parsing the source would print garbage or fail.
    cat <<'USAGE'
Theta installer.

  curl -fsSL https://raw.githubusercontent.com/Bhanu4417/theta/main/install.sh | sh

Downloads the release for this platform, verifies its SHA-256, and installs
the binary. Nothing else is touched.

Environment:
  THETA_VERSION      version to install, e.g. 0.1.0 (default: latest release)
  THETA_INSTALL_DIR  destination directory (default: ~/.local/bin)
  THETA_BASE_URL     release download base (used by the tests)

Flags:
  --version <v>   install a specific version
  --dir <path>    install somewhere else
  --dry-run       report what would happen, download nothing
  --help          this text
USAGE
    exit 0
}

while [ $# -gt 0 ]; do
    case "$1" in
        --version) [ $# -ge 2 ] || die "--version needs a value"; VERSION="$2"; shift 2 ;;
        --dir)     [ $# -ge 2 ] || die "--dir needs a value";     INSTALL_DIR="$2"; shift 2 ;;
        --dry-run) DRY_RUN=1; shift ;;
        --help|-h) usage ;;
        *) die "unknown option: $1 (try --help)" ;;
    esac
done

[ -n "$INSTALL_DIR" ] || INSTALL_DIR="$HOME/.local/bin"

# --- platform ---------------------------------------------------------------

uname_s=$(uname -s 2>/dev/null || echo unknown)
uname_m=$(uname -m 2>/dev/null || echo unknown)

case "$uname_s" in
    Linux)  os=linux ;;
    Darwin) os=darwin ;;
    *) die "unsupported OS: $uname_s. On Windows use install.ps1, or build from source." ;;
esac

case "$uname_m" in
    x86_64|amd64) arch=x64 ;;
    arm64|aarch64) arch=arm64 ;;
    *) die "unsupported architecture: $uname_m" ;;
esac

case "$os-$arch" in
    linux-x64)   target="x86_64-unknown-linux-gnu" ;;
    linux-arm64) target="aarch64-unknown-linux-gnu" ;;
    darwin-arm64) target="aarch64-apple-darwin" ;;
    darwin-x64)  target="x86_64-apple-darwin" ;;
    *) die "no prebuilt binary for $os-$arch" ;;
esac

# --- downloader -------------------------------------------------------------

if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL "$1" -o "$2"; }
    fetch_stdout() { curl -fsSL "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -q -O "$2" "$1"; }
    fetch_stdout() { wget -q -O- "$1"; }
else
    die "needs curl or wget"
fi

# --- version ----------------------------------------------------------------

if [ -z "$VERSION" ]; then
    say "Resolving the latest version…"
    # The redirect from /releases/latest ends in the tag.
    latest=$(fetch_stdout "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null \
        | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"v\{0,1\}\([^"]*\)".*/\1/p' | head -n1)
    [ -n "$latest" ] || die "could not determine the latest version; set THETA_VERSION"
    VERSION="$latest"
fi
VERSION="${VERSION#v}"

ASSET="$BIN-$target.tar.gz"
BASE="${THETA_BASE_URL:-https://github.com/$REPO/releases/download}"
URL="$BASE/v$VERSION/$ASSET"

say "$BIN_NAME $VERSION"
say "  platform: $os/$arch ($target)"
say "  from:     $URL"
say "  into:     $INSTALL_DIR/$BIN"

if [ "$DRY_RUN" -eq 1 ]; then
    say "dry run: nothing downloaded"
    exit 0
fi

# --- download and verify ----------------------------------------------------

tmp=$(mktemp -d 2>/dev/null || mktemp -d -t theta) || die "cannot create a temp directory"
trap 'rm -rf "$tmp"' EXIT INT TERM

say "Downloading…"
if ! fetch "$URL" "$tmp/$ASSET"; then
    # A 404 is ambiguous on GitHub: either the release does not exist, or the
    # repository is private and the request was not authenticated.
    cat >&2 <<EOF
error: could not download $ASSET

  $URL

This usually means one of:
  * the release does not exist yet. Check:
      https://github.com/$REPO/releases
  * there is no network access to github.com.
  * the repository is not publicly readable. Anonymous downloads only work for
    public repositories; build from source instead:
      git clone https://github.com/$REPO && cd theta && cargo build --release
EOF
    exit 1
fi

say "Verifying checksum…"
if fetch "$URL.sha256" "$tmp/$ASSET.sha256" 2>/dev/null; then
    expected=$(awk '{print $1}' "$tmp/$ASSET.sha256" | head -n1)
    if command -v sha256sum >/dev/null 2>&1; then
        actual=$(sha256sum "$tmp/$ASSET" | awk '{print $1}')
    elif command -v shasum >/dev/null 2>&1; then
        actual=$(shasum -a 256 "$tmp/$ASSET" | awk '{print $1}')
    else
        actual=""
        warn "warning: no sha256sum or shasum; skipping verification"
    fi
    if [ -n "$actual" ]; then
        [ "$expected" = "$actual" ] || die "checksum mismatch
  expected $expected
  got      $actual
Do not use this download."
        say "  ok"
    fi
else
    warn "warning: no checksum published for this release; skipping verification"
fi

# --- install ----------------------------------------------------------------

say "Extracting…"
tar xzf "$tmp/$ASSET" -C "$tmp" || die "could not extract $ASSET"
[ -f "$tmp/$BIN" ] || die "the archive did not contain $BIN"

mkdir -p "$INSTALL_DIR" || die "cannot create $INSTALL_DIR"
# Write beside the target and rename, so a running binary is never truncated.
install_tmp="$INSTALL_DIR/.$BIN.new.$$"
cp "$tmp/$BIN" "$install_tmp" || die "cannot write to $INSTALL_DIR"
chmod 755 "$install_tmp"
mv -f "$install_tmp" "$INSTALL_DIR/$BIN" || die "cannot replace $INSTALL_DIR/$BIN"

say "Installed $INSTALL_DIR/$BIN"

# --- PATH -------------------------------------------------------------------

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        say ""
        say "$INSTALL_DIR is not on your PATH. Add it:"
        shell_name=$(basename "${SHELL:-sh}")
        case "$shell_name" in
            fish) say "  fish_add_path $INSTALL_DIR" ;;
            zsh)  say "  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.zshrc" ;;
            bash) say "  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.bashrc" ;;
            *)    say "  export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
        esac
        ;;
esac

say ""
say "Run '$BIN --help' to get started, then '$BIN' to open a workspace."
