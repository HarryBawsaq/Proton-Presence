#!/usr/bin/env bash
# Builds proton-presence and installs it as a systemd user service.
set -euo pipefail

cd "$(dirname "$(readlink -f "$0")")"

unit=proton-presence.service
bin_dir="$HOME/.local/bin"
data_dir="$HOME/.local/share/proton-presence"
helper=proton-presence-bridge.exe
unit_dir="$HOME/.config/systemd/user"
config_dir="$HOME/.config/proton-presence"

if [ "$(id -u)" -eq 0 ]; then
    echo "Run this as your normal user, not with sudo: it installs a user service." >&2
    exit 1
fi
# How to install a missing package on this distribution.
install_hint() {
    if command -v pacman >/dev/null; then
        echo "sudo pacman -S --needed $1"
    elif command -v apt >/dev/null; then
        echo "sudo apt install $2"
    elif command -v dnf >/dev/null; then
        echo "sudo dnf install $3"
    else
        echo "install it with your distribution's package manager"
    fi
}
if ! command -v cargo >/dev/null; then
    echo "cargo (Rust) not found. Install it first: $(install_hint rust cargo cargo)" >&2
    exit 1
fi
if ! command -v curl >/dev/null; then
    echo "curl not found. Install it first: $(install_hint curl curl curl)" >&2
    exit 1
fi
if ! systemctl --user show-environment >/dev/null 2>&1; then
    echo "Cannot reach the systemd user manager (systemctl --user)." >&2
    echo "Run this from your normal desktop login session." >&2
    exit 1
fi

echo "==> Building (release)"
cargo build --release --locked

echo "==> Installing $bin_dir/proton-presence"
install -Dm755 target/release/proton-presence "$bin_dir/proton-presence"

# The helper that runs inside a game's Wine prefix is a Windows program.
# A prebuilt copy is in the repository; it is rebuilt from bridge/bridge.c
# when a Windows cross-compiler (mingw-w64) is installed.
if command -v x86_64-w64-mingw32-gcc >/dev/null; then
    echo "==> Building the bridge helper from source"
    bridge/build.sh
else
    echo "==> Using the prebuilt bridge helper"
fi
echo "==> Installing $data_dir/$helper"
install -Dm644 "bridge/$helper" "$data_dir/$helper"

echo "==> Installing $unit_dir/$unit"
install -Dm644 "$unit" "$unit_dir/$unit"

if [ -e "$config_dir/config.toml" ]; then
    echo "==> Keeping existing $config_dir/config.toml"
else
    echo "==> Creating $config_dir/config.toml"
    install -Dm644 config.example.toml "$config_dir/config.toml"
fi

echo "==> Enabling and starting the service"
systemctl --user daemon-reload
systemctl --user enable "$unit"
# restart rather than start, so a reinstall picks up the new binary
systemctl --user restart "$unit"

echo
"$bin_dir/proton-presence" status
echo
echo "Installed. Nothing else to do: the service starts at every login and"
echo "shows your Proton games in Discord."
echo "Optional settings: $config_dir/config.toml"
echo "Logs: journalctl --user -u proton-presence -f"
