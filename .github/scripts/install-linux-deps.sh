#!/usr/bin/env bash
# Build dependencies of Servo (and therefore FerroSurf) on Debian/Ubuntu.
set -euo pipefail
sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  build-essential clang llvm cmake python3 pkg-config curl \
  libx11-dev libxcb1-dev libxkbcommon-dev libxkbcommon-x11-0 libwayland-dev \
  libfontconfig1-dev libfreetype6-dev libgl1-mesa-dev libegl1-mesa-dev \
  libudev-dev libdbus-1-dev libssl-dev
