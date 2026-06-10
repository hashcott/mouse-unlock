#!/usr/bin/env bash
# Install mouse-unlock: build (Rust) -> copy binary -> register systemd service.
set -euo pipefail

BIN_NAME="mouse-unlock"
SETUP_NAME="mouse-unlock-setup"
BIN_DEST="/usr/local/bin/${BIN_NAME}"
SETUP_DEST="/usr/local/bin/${SETUP_NAME}"
CONF_DEST="/etc/mouse-unlock.conf"
SERVICE_DEST="/etc/systemd/system/mouse-unlock.service"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [[ $EUID -ne 0 ]]; then
  echo "Must run as root:  sudo bash install.sh" >&2
  exit 1
fi

# cargo may live in the calling user's ~/.cargo/bin under sudo.
if ! command -v cargo >/dev/null 2>&1; then
  if [[ -n "${SUDO_USER:-}" ]] && [[ -x "/home/${SUDO_USER}/.cargo/bin/cargo" ]]; then
    export PATH="/home/${SUDO_USER}/.cargo/bin:${PATH}"
  fi
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo "'cargo' not found. Install Rust: https://rustup.rs" >&2
  exit 1
fi

echo "==> Building (release)..."
( cd "$SCRIPT_DIR" && cargo build --release )

echo "==> Installing daemon -> ${BIN_DEST}"
install -m 0755 "${SCRIPT_DIR}/target/release/${BIN_NAME}" "${BIN_DEST}"

echo "==> Installing setup TUI -> ${SETUP_DEST}"
install -m 0755 "${SCRIPT_DIR}/target/release/${SETUP_NAME}" "${SETUP_DEST}"

if [[ -f "$CONF_DEST" ]]; then
  echo "==> Keeping existing config: ${CONF_DEST}"
else
  echo "==> Installing default config -> ${CONF_DEST}"
  install -m 0600 "${SCRIPT_DIR}/mouse-unlock.conf" "${CONF_DEST}"
fi

echo "==> Installing service -> ${SERVICE_DEST}"
install -m 0644 "${SCRIPT_DIR}/mouse-unlock.service" "${SERVICE_DEST}"

echo "==> Enabling service"
systemctl daemon-reload
systemctl enable --now mouse-unlock.service

echo
echo "Almost done! Now set your click pattern (the daemon won't unlock until you do):"
echo "  sudo mouse-unlock-setup"
echo
echo "Check it with:"
echo "  systemctl status mouse-unlock"
echo "  journalctl -u mouse-unlock -f"
