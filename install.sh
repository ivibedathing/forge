#!/bin/sh
# Install the `engine` binary from a GitHub release.
#
#   curl -fsSL https://raw.githubusercontent.com/ivibedathing/forge/main/install.sh | sh
#
# Environment:
#   FORGE_VERSION      tag to install (default: the latest release)
#   FORGE_INSTALL_DIR  where the binary lands (default: ~/.local/bin)
#
# POSIX sh on purpose: this runs before anything is installed, on whatever
# shell the machine has.

set -eu

REPO=${FORGE_REPO:-ivibedathing/forge}
INSTALL_DIR=${FORGE_INSTALL_DIR:-$HOME/.local/bin}

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "$1 is required but not installed"; }

need uname
need tar
if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL "$1"; }
    fetch_to() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -qO- "$1"; }
    fetch_to() { wget -qO "$2" "$1"; }
else
    die "neither curl nor wget is installed"
fi

os=$(uname -s)
arch=$(uname -m)
case "$os-$arch" in
    Darwin-arm64)          target=aarch64-apple-darwin ;;
    Darwin-x86_64)         target=x86_64-apple-darwin ;;
    Linux-x86_64)          target=x86_64-unknown-linux-gnu ;;
    Linux-aarch64)
        die "no prebuilt binary for Linux arm64 yet — build from source:
  cargo install --git https://github.com/$REPO engine-cli --locked" ;;
    *)
        die "unsupported platform $os-$arch — build from source:
  cargo install --git https://github.com/$REPO engine-cli --locked" ;;
esac

version=${FORGE_VERSION:-}
if [ -z "$version" ]; then
    say "Resolving the latest release of $REPO..."
    # sed rather than jq: jq is not something a machine is guaranteed to have
    # before anything is installed, and one well-known field is worth pulling
    # out by hand.
    version=$(
        fetch "https://api.github.com/repos/$REPO/releases/latest" |
        sed -n 's/.*"tag_name" *: *"\([^"]*\)".*/\1/p' |
        head -n 1
    )
    [ -n "$version" ] || die "could not determine the latest release; set FORGE_VERSION"
fi

name="engine-$version-$target"
url="https://github.com/$REPO/releases/download/$version/$name.tar.gz"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

say "Downloading $name..."
fetch_to "$url" "$tmp/$name.tar.gz" || die "download failed: $url"

# Verify against the release's SHA256SUMS when a checksum tool exists. A
# missing tool is a warning, not a failure — refusing to install because the
# machine has no shasum helps nobody.
if fetch "https://github.com/$REPO/releases/download/$version/SHA256SUMS" > "$tmp/SHA256SUMS" 2>/dev/null; then
    expected=$(sed -n "s/^\([0-9a-f]\{64\}\)  *$name\.tar\.gz\$/\1/p" "$tmp/SHA256SUMS" | head -n 1)
    if [ -n "$expected" ]; then
        if command -v shasum >/dev/null 2>&1; then
            actual=$(shasum -a 256 "$tmp/$name.tar.gz" | cut -d' ' -f1)
        elif command -v sha256sum >/dev/null 2>&1; then
            actual=$(sha256sum "$tmp/$name.tar.gz" | cut -d' ' -f1)
        else
            actual=""
            say "warning: no shasum or sha256sum found; skipping checksum verification"
        fi
        if [ -n "$actual" ] && [ "$actual" != "$expected" ]; then
            die "checksum mismatch for $name.tar.gz (expected $expected, got $actual)"
        fi
    fi
fi

tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
mkdir -p "$INSTALL_DIR"
install_path="$INSTALL_DIR/engine"
# mv, not cp: replacing a running binary in place is what breaks a live
# process, and mv onto the same filesystem swaps the directory entry instead.
mv -f "$tmp/$name/engine" "$install_path"
chmod +x "$install_path"

say ""
say "Installed $version to $install_path"

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        say ""
        say "$INSTALL_DIR is not on your PATH. Add it:"
        say "    export PATH=\"$INSTALL_DIR:\$PATH\""
        ;;
esac

say ""
say "Next:"
say "    engine init my-scene && cd my-scene"
say "    engine screenshot first.json --out /tmp/first.png --steps 120"
say ""
say "engine agent-guide prints the orientation for a coding agent."
say "Rendering needs a GPU with a Vulkan, Metal, or DX12 backend; engine info"
say "reports which adapter was selected."
