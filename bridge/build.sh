#!/usr/bin/env bash
# Builds the Windows bridge helper. Needs the mingw-w64 cross-compiler:
#   Arch: mingw-w64-gcc   Debian/Ubuntu: gcc-mingw-w64-x86-64   Fedora: mingw64-gcc
set -euo pipefail
cd "$(dirname "$(readlink -f "$0")")"
x86_64-w64-mingw32-gcc -Os -s -Wall -Wextra -Wl,--no-insert-timestamp \
    -o proton-presence-bridge.exe bridge.c -lws2_32
