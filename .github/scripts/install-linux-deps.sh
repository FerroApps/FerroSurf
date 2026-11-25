#!/usr/bin/env bash
# Build dependencies of Servo (and therefore FerroSurf) on Debian/Ubuntu.
# Ubuntu 22.04 ships clang 14, whose headers break the bindgen run of Servo's
# mozangle (WebGL); LLVM 18 comes from apt.llvm.org and is used for libclang.
set -euo pipefail
LLVM=18
sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  build-essential cmake python3 pkg-config curl lsb-release wget gnupg software-properties-common \
  libx11-dev libxcb1-dev libxkbcommon-dev libxkbcommon-x11-0 libwayland-dev \
  libfontconfig1-dev libfreetype6-dev libgl1-mesa-dev libegl1-mesa-dev \
  libudev-dev libdbus-1-dev libssl-dev
if ! command -v "clang-$LLVM" >/dev/null 2>&1; then
  wget -qO /tmp/llvm.sh https://apt.llvm.org/llvm.sh
  sudo bash /tmp/llvm.sh "$LLVM"
fi
sudo apt-get install -y --no-install-recommends "libclang-$LLVM-dev" "clang-$LLVM" "llvm-$LLVM"
if [ -n "${GITHUB_ENV:-}" ]; then
  {
    echo "LIBCLANG_PATH=/usr/lib/llvm-$LLVM/lib"
    echo "CLANG_PATH=/usr/bin/clang-$LLVM"
  } >> "$GITHUB_ENV"
  echo "/usr/lib/llvm-$LLVM/bin" >> "$GITHUB_PATH"
fi
