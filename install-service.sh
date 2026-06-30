#!/usr/bin/env bash
set -euo pipefail

# ── Configuration ──────────────────────────────────────────────────────────
BINARY="/home/jahanzaib/ali-kernel/target/release/ali-kernel"
SERVICE_FILE="/home/jahanzaib/ali-kernel/ali-kernel.service"
SYSTEMD_DIR="/etc/systemd/system"
USER="jahanzaib"
WORKING_DIR="/home/jahanzaib/ali-kernel"

# ── Helpers ────────────────────────────────────────────────────────────────
green()  { printf "\033[0;32m%s\033[0m\n" "$*"; }
yellow() { printf "\033[0;33m%s\033[0m\n" "$*"; }
red()    { printf "\033[0;31m%s\033[0m\n" "$*"; }
cyan()   { printf "\033[0;36m%s\033[0m\n" "$*"; }

# ── Pre-flight checks ─────────────────────────────────────────────────────
if [[ $EUID -eq 0 ]]; then
    cyan "Running as root — proceeding without sudo."
else
    if ! command -v sudo &>/dev/null; then
        red "sudo is not available. Please run this script as root."
        exit 1
    fi
    cyan "Running as non-root — using sudo."
fi

if [[ ! -f "$BINARY" ]]; then
    red "Binary not found: $BINARY"
    exit 1
fi

if [[ ! -f "$SERVICE_FILE" ]]; then
    red "Service file not found: $SERVICE_FILE"
    exit 1
fi

# ── Install ────────────────────────────────────────────────────────────────
green "Copying service file to $SYSTEMD_DIR ..."
sudo cp "$SERVICE_FILE" "$SYSTEMD_DIR/ali-kernel.service"

green "Reloading systemd daemon ..."
sudo systemctl daemon-reload

green "Enabling service ..."
sudo systemctl enable --now ali-kernel

# ── Status ─────────────────────────────────────────────────────────────────
cyan "Service status:"
sudo systemctl status ali-kernel --no-pager

green "Ali Kernel service installed and started successfully."
