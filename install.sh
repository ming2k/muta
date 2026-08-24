#!/usr/bin/env bash
#
# install.sh — one-line installer for muta.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/ming2k/muta/main/install.sh | bash
#
# Or, to pin a version:
#   MUTA_VERSION=0.10.0 curl -fsSL .../install.sh | bash
#
# Installs the `muta` core and `mutx` terminal app into ~/.local/bin (or
# $INSTALL_DIR if set).
# Detects OS + architecture and pulls the matching release tarball from GitHub.

set -euo pipefail

# --- config -------------------------------------------------------------

REPO="ming2k/muta"
# Both binaries are required for local use: mutx starts the sibling muta core
# on demand.
BIN_NAMES=(muta mutx)
# Where the binaries land. Honour an explicit override, otherwise ~/.local/bin
# (no sudo needed; create it if missing).
INSTALL_DIR="${INSTALL_DIR:-${HOME}/.local/bin}"
# Pin a version with MUTA_VERSION="0.10.0". Empty means "latest release".
MUTA_VERSION="${MUTA_VERSION:-}"

# --- pretty printing ----------------------------------------------------

if [[ -n "${NO_COLOR:-}" ]] || [[ ! -t 1 ]]; then
    fmt_reset=""; fmt_bold=""; fmt_green=""; fmt_red=""; fmt_yellow=""; fmt_blue=""
else
    fmt_reset=$'\033[0m'; fmt_bold=$'\033[1m'
    fmt_green=$'\033[32m'; fmt_red=$'\033[31m'
    fmt_yellow=$'\033[33m'; fmt_blue=$'\033[34m'
fi

info()  { printf "${fmt_blue}›${fmt_reset} %s\n" "$*"; }
good()  { printf "${fmt_green}✓${fmt_reset} %s\n" "$*"; }
warn()  { printf "${fmt_yellow}!${fmt_reset} %s\n" "$*" >&2; }
abort() { printf "${fmt_red}✗${fmt_reset} %s\n" "$*" >&2; exit 1; }

# --- prerequisites ------------------------------------------------------

need() { command -v "$1" >/dev/null 2>&1 || abort "Required command not found: $1"; }
need uname
need tar

# Pick an HTTP fetcher. Prefer curl (matches the documented pipe-to-bash flow).
if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL "$1"; }        # to stdout
    fetch_to() { curl -fsSL -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -qO- "$1"; }
    fetch_to() { wget -qO "$2" "$1"; }
else
    abort "Neither curl nor wget is installed. Please install one and retry."
fi

# --- detect platform ----------------------------------------------------

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
    Darwin) target_os="apple-darwin" ;;
    Linux)  target_os="unknown-linux-gnu" ;;
    *)      abort "Unsupported OS: $os (only macOS and Linux are packaged)." ;;
esac

case "$arch" in
    x86_64|amd64)    target_arch="x86_64" ;;
    aarch64|arm64)   target_arch="aarch64" ;;
    *)               abort "Unsupported architecture: $arch." ;;
esac

# Alpine (musl) gets the static build so the binary isn't pinned to a glibc.
if [[ "$target_os" == "unknown-linux-gnu" && "$target_arch" == "x86_64" ]] \
   && { [[ -f /etc/alpine-release ]] || ldd --version 2>&1 | grep -qi musl; }; then
    target_os="unknown-linux-musl"
fi

target="${target_arch}-${target_os}"
info "Detected ${fmt_bold}${target}${fmt_reset}"

# --- resolve version ----------------------------------------------------

if [[ -z "$MUTA_VERSION" ]]; then
    info "Looking up the latest release…"
    MUTA_VERSION="$(fetch "https://api.github.com/repos/${REPO}/releases/latest" \
        | grep -m1 '"tag_name"' | sed -E 's/.*"v?([^"]+)".*/\1/')"
    [[ -n "$MUTA_VERSION" ]] || abort "Could not determine the latest release."
fi
# Allow the user to pass either "0.10.0" or "v0.10.0".
version="${MUTA_VERSION#v}"
info "Installing ${fmt_bold}muta v${version}${fmt_reset}"

# --- download + extract -------------------------------------------------

tarball="muta-${version}-${target}.tar.gz"
url="https://github.com/${REPO}/releases/download/v${version}/${tarball}"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

info "Downloading $url"
fetch_to "$url" "${tmpdir}/${tarball}"
fetch_to "${url}.sha256" "${tmpdir}/${tarball}.sha256"

info "Verifying SHA-256 checksum…"
expected="$(awk '{print $1}' "${tmpdir}/${tarball}.sha256")"
if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "${tmpdir}/${tarball}" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "${tmpdir}/${tarball}" | awk '{print $1}')"
else
    abort "Neither sha256sum nor shasum is installed; refusing an unverified install."
fi
[[ "$actual" == "$expected" ]] || abort "SHA-256 checksum mismatch; download was not installed."

info "Extracting…"
tar -xzf "${tmpdir}/${tarball}" -C "$tmpdir"

# --- install ------------------------------------------------------------

mkdir -p "$INSTALL_DIR"
for bin_name in "${BIN_NAMES[@]}"; do
    src="$(find "$tmpdir" -type f -name "$bin_name" -perm -u+x | head -n1)"
    [[ -n "$src" ]] || abort "Binary '$bin_name' not found inside the archive."
    dest="${INSTALL_DIR%/}/${bin_name}"
    install -m 0755 "$src" "$dest"
    good "Installed ${fmt_bold}${dest}${fmt_reset}"
done

# --- PATH sanity check --------------------------------------------------

case ":${PATH:-}:" in
    *":${INSTALL_DIR}:"*) ;;
    *)
        warn "$INSTALL_DIR is not on your PATH."
        printf "  Add this to your shell profile (~/.bashrc, ~/.zshrc, …):\n"
        printf '    export PATH="%s:$PATH"\n' "$INSTALL_DIR"
        ;;
esac

# Shell-completion hint: the binary can print its own completions, but that
# is left to the user. Finish with a friendly next-step.
printf "\n"
good "Done! Run ${fmt_bold}mutx${fmt_reset} to start."
printf "  First launch: press ${fmt_bold}Ctrl+M${fmt_reset} to pick a provider.\n"
