#!/usr/bin/env bash
# One-line installer for mouse-unlock — downloads a prebuilt release (no Rust needed)
# and sets up the systemd service.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/OWNER/REPO/master/scripts/install.sh | sudo bash
#
# Options (environment variables):
#   REPO=owner/repo     GitHub repository to install from (required if not baked in below)
#   VERSION=v0.2.0      Specific release tag (default: latest)
#
#   curl -fsSL .../install.sh | sudo REPO=owner/repo bash
set -euo pipefail

REPO="${REPO:-OWNER/REPO}"
VERSION="${VERSION:-latest}"
BIN_DIR="/usr/local/bin"
CONF_DEST="/etc/mouse-unlock.conf"
SERVICE_DEST="/etc/systemd/system/mouse-unlock.service"

err() {
  echo "Error: $*" >&2
  exit 1
}

[[ $EUID -eq 0 ]] || err "must run as root (use sudo)"
[[ "$REPO" != "OWNER/REPO" ]] || err "set the repository, e.g.  REPO=owner/repo  (or edit this script)"

# Only Linux x86_64 binaries are published; other targets must build from source.
os="$(uname -s)"
arch="$(uname -m)"
[[ "$os" == "Linux" ]] || err "only Linux is supported (got: $os)"
[[ "$arch" == "x86_64" || "$arch" == "amd64" ]] || err "only x86_64 is supported (got: $arch) — build from source instead"

command -v curl >/dev/null || err "curl is required"
command -v tar >/dev/null || err "tar is required"
command -v systemctl >/dev/null || err "systemd (systemctl) is required"

if [[ "$VERSION" == "latest" ]]; then
  api="https://api.github.com/repos/${REPO}/releases/latest"
else
  api="https://api.github.com/repos/${REPO}/releases/tags/${VERSION}"
fi

echo "==> Looking up release: ${REPO} (${VERSION})"
json="$(curl -fsSL "$api")" || err "cannot fetch release info from GitHub"

pick_url() { # $1 = suffix regex
  printf '%s' "$json" \
    | grep -oE "\"browser_download_url\": *\"[^\"]*${1}\"" \
    | head -n1 \
    | sed -E 's/.*"(https[^"]+)"/\1/'
}

url="$(pick_url 'linux-x86_64\.tar\.gz')"
[[ -n "$url" ]] || err "no linux-x86_64 asset found in the release"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
tarball="${tmp}/mouse-unlock.tar.gz"

echo "==> Downloading: $url"
curl -fsSL "$url" -o "$tarball"

sha_url="$(pick_url 'linux-x86_64\.tar\.gz\.sha256')"
if [[ -n "$sha_url" ]] && command -v sha256sum >/dev/null; then
  echo "==> Verifying checksum"
  want="$(curl -fsSL "$sha_url" | awk '{print $1}')"
  got="$(sha256sum "$tarball" | awk '{print $1}')"
  [[ "$want" == "$got" ]] || err "checksum mismatch (expected $want, got $got)"
fi

echo "==> Extracting"
tar -xzf "$tarball" -C "$tmp"
src="$(find "$tmp" -maxdepth 1 -type d -name 'mouse-unlock-*-linux-x86_64' | head -n1)"
[[ -n "$src" ]] || err "unexpected archive layout"

echo "==> Installing binaries -> ${BIN_DIR}"
install -m 0755 "${src}/mouse-unlock" "${BIN_DIR}/mouse-unlock"
install -m 0755 "${src}/mouse-unlock-setup" "${BIN_DIR}/mouse-unlock-setup"

if [[ -f "$CONF_DEST" ]]; then
  echo "==> Keeping existing config: ${CONF_DEST}"
else
  echo "==> Installing default config -> ${CONF_DEST}"
  install -m 0644 "${src}/mouse-unlock.conf" "${CONF_DEST}"
fi

echo "==> Installing service -> ${SERVICE_DEST}"
install -m 0644 "${src}/mouse-unlock.service" "${SERVICE_DEST}"

echo "==> Enabling service"
systemctl daemon-reload
systemctl enable --now mouse-unlock.service

cat <<'EOF'

Done! mouse-unlock is installed and running.
  Configure:  sudo mouse-unlock-setup
  Status:     systemctl status mouse-unlock
  Logs:       journalctl -u mouse-unlock -f
EOF
