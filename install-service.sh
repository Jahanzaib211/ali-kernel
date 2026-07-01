#!/usr/bin/env bash
set -euo pipefail

# ── Configuration ──────────────────────────────────────────────────────────
BINARY="/home/jahanzaib/ali-kernel/target/release/ali-kernel"
KERNEL_SERVICE="/home/jahanzaib/ali-kernel/ali-kernel.service"
LLAMA_SERVICE="/home/jahanzaib/ali-kernel/llama-server.service"
SYSTEMD_DIR="/etc/systemd/system"

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
    red "Binary not found: $BINARY (run: cargo build --release)"
    exit 1
fi

if [[ ! -f "$KERNEL_SERVICE" || ! -f "$LLAMA_SERVICE" ]]; then
    red "Service file(s) not found: $KERNEL_SERVICE / $LLAMA_SERVICE"
    exit 1
fi

# ── Install ────────────────────────────────────────────────────────────────
green "Copying service files to $SYSTEMD_DIR ..."
sudo cp "$LLAMA_SERVICE" "$SYSTEMD_DIR/llama-server.service"
sudo cp "$KERNEL_SERVICE" "$SYSTEMD_DIR/ali-kernel.service"

green "Reloading systemd daemon ..."
sudo systemctl daemon-reload

green "Enabling services (llama-server first, ali-kernel depends on it) ..."
sudo systemctl enable --now llama-server
sudo systemctl enable --now ali-kernel

# ── Status ─────────────────────────────────────────────────────────────────
cyan "llama-server status:"
sudo systemctl status llama-server --no-pager
cyan "ali-kernel status:"
sudo systemctl status ali-kernel --no-pager

green "llama-server + Ali Kernel installed and will auto-start on every boot."
