#!/bin/sh
# install.sh — install rlph from GitHub Releases
# Usage: curl -fsSL https://raw.githubusercontent.com/hsubra89/rlph/main/install.sh | sh
set -eu

REPO="hsubra89/rlph"
INSTALL_DIR="${RLPH_INSTALL_DIR:-$HOME/.rlph/bin}"

# --- helpers ----------------------------------------------------------------

has_cmd() { command -v "$1" >/dev/null 2>&1; }

# Colored output (only when connected to a terminal)
if [ -t 1 ]; then
    bold="\033[1m"
    green="\033[32m"
    red="\033[31m"
    cyan="\033[36m"
    reset="\033[0m"
else
    bold="" green="" red="" cyan="" reset=""
fi

info()  { printf "${cyan}info${reset}  %s\n" "$1"; }
ok()    { printf "${green}ok${reset}    %s\n" "$1"; }
err()   { printf "${red}error${reset} %s\n" "$1" >&2; }

abort() { err "$1"; exit 1; }

# --- detect platform --------------------------------------------------------

detect_os() {
    case "$(uname -s)" in
        Linux*)  echo "linux" ;;
        Darwin*) echo "darwin" ;;
        *)       abort "Unsupported OS: $(uname -s)" ;;
    esac
}

detect_arch() {
    case "$(uname -m)" in
        x86_64|amd64)    echo "x86_64" ;;
        aarch64|arm64)   echo "aarch64" ;;
        *)               abort "Unsupported architecture: $(uname -m)" ;;
    esac
}

target_triple() {
    _os="$1"
    _arch="$2"
    case "$_os" in
        darwin) echo "${_arch}-apple-darwin" ;;
        linux)  echo "${_arch}-unknown-linux-gnu" ;;
    esac
}

# --- download helper --------------------------------------------------------

fetch() {
    _url="$1"
    _out="$2"
    if has_cmd curl; then
        curl -fsSL -o "$_out" "$_url"
    elif has_cmd wget; then
        wget -qO "$_out" "$_url"
    else
        abort "Neither curl nor wget found. Install one and retry."
    fi
}

fetch_stdout() {
    _url="$1"
    if has_cmd curl; then
        curl -fsSL "$_url"
    elif has_cmd wget; then
        wget -qO- "$_url"
    else
        abort "Neither curl nor wget found. Install one and retry."
    fi
}

# --- main -------------------------------------------------------------------

main() {
    os="$(detect_os)"
    arch="$(detect_arch)"
    target="$(target_triple "$os" "$arch")"

    info "Detected platform: ${target}"

    # Fetch latest release tag
    info "Fetching latest release..."
    release_json="$(fetch_stdout "https://api.github.com/repos/${REPO}/releases/latest")"

    # Parse tag_name without jq (portable)
    tag="$(printf '%s' "$release_json" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/')"
    [ -z "$tag" ] && abort "Could not determine latest release tag."
    info "Latest release: ${bold}${tag}${reset}"

    # Build archive URL
    archive="rlph-${tag}-${target}.tar.gz"
    url="https://github.com/${REPO}/releases/download/${tag}/${archive}"

    # Temp dir with cleanup
    tmpdir="$(mktemp -d)"
    trap 'rm -rf "$tmpdir"' EXIT

    info "Downloading ${archive}..."
    fetch "$url" "${tmpdir}/${archive}"

    info "Extracting..."
    tar -xzf "${tmpdir}/${archive}" -C "$tmpdir"

    # Install binary (archive contains a subdirectory)
    extracted_dir="${tmpdir}/rlph-${tag}-${target}"
    mkdir -p "$INSTALL_DIR"
    mv "${extracted_dir}/rlph" "${INSTALL_DIR}/rlph"
    chmod +x "${INSTALL_DIR}/rlph"

    ok "Installed rlph ${tag} to ${INSTALL_DIR}/rlph"

    # PATH hint
    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*) ;;
        *)
            printf "\n"
            info "Add rlph to your PATH by adding this to your shell profile:"
            printf "\n  ${bold}export PATH=\"%s:\$PATH\"${reset}\n\n" "$INSTALL_DIR"
            ;;
    esac
}

main
