#!/usr/bin/env bash
# Install FerroSurf from an extracted release tarball or a source checkout.
# Usage: ./install.sh [path/to/ferrosurf-binary]
#   PREFIX=/usr/local ./install.sh   (system-wide, needs root)
#   default PREFIX=$HOME/.local
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
PREFIX="${PREFIX:-$HOME/.local}"

# Locate binary: argument, next to this script's release layout, or cargo target.
BIN="${1:-}"
for c in "$BIN" "$ROOT/ferrosurf" "$ROOT/target/release/ferrosurf" "$HERE/../ferrosurf"; do
  if [ -n "$c" ] && [ -f "$c" ]; then BIN="$c"; break; fi
done
[ -f "${BIN:-}" ] || { echo "ferrosurf binary not found; pass its path as the first argument" >&2; exit 1; }

# Locate resources directory.
RES=""
for c in "$ROOT/resources" "$ROOT/crates/ferrosurf/resources"; do
  [ -d "$c" ] && { RES="$c"; break; }
done
[ -n "$RES" ] || { echo "resources directory not found" >&2; exit 1; }

install -Dm755 "$BIN" "$PREFIX/bin/ferrosurf"
# Resources live next to the binary: <bin dir>/resources
rm -rf "$PREFIX/bin/resources"
cp -r "$RES" "$PREFIX/bin/resources"

install -Dm644 "$HERE/org.ferroapps.FerroSurf.desktop" "$PREFIX/share/applications/org.ferroapps.FerroSurf.desktop"
install -Dm644 "$HERE/org.ferroapps.FerroSurf.metainfo.xml" "$PREFIX/share/metainfo/org.ferroapps.FerroSurf.metainfo.xml"
for s in 16 32 48 64 128 256 512; do
  install -Dm644 "$RES/icons/ferrosurf-$s.png" "$PREFIX/share/icons/hicolor/${s}x${s}/apps/ferrosurf.png"
done
install -Dm644 "$RES/icons/ferrosurf.svg" "$PREFIX/share/icons/hicolor/scalable/apps/ferrosurf.svg"

command -v update-desktop-database >/dev/null && update-desktop-database "$PREFIX/share/applications" || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "$PREFIX/share/icons/hicolor" || true

echo "FerroSurf installed to $PREFIX/bin/ferrosurf"
case ":$PATH:" in *":$PREFIX/bin:"*) ;; *) echo "Note: add $PREFIX/bin to your PATH";; esac
