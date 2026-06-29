#!/usr/bin/env bash
set -euo pipefail

# ── Configuration ──────────────────────────────────────────────────────────
BINARY="/home/jahanzaib/ornith-stack/target/release/ornith-gateway"
SERVICE_FILE="/home/jahanzaib/ornith-gateway.service"
SYSTEMD_DIR="/etc/systemd/system"
USER="jahanzaib"
WORKING_DIR="/home/jahanzaib/ornith-stack"

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
sudo cp "$SERVICE_FILE" "$SYSTEMD_DIR/ornith-gateway.service"

green "Reloading systemd daemon ..."
sudo systemctl daemon-reload

green "Enabling service ..."
sudo systemctl enable --now ornith-gateway

# ── Status ─────────────────────────────────────────────────────────────────
cyan "Service status:"
sudo systemctl status ornith-gateway --no-pager

green "Ornith Sovereign Gateway service installed and started successfully."
