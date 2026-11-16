#!/usr/bin/env bash
# Builds the Windows bridge helper. Needs mingw-w64 (pacman -S mingw-w64-gcc).
set -euo pipefail
cd "$(dirname "$(readlink -f "$0")")"
x86_64-w64-mingw32-gcc -Os -s -Wall -Wextra -Wl,--no-insert-timestamp \
    -o proton-presence-bridge.exe bridge.c -lws2_32
