#!/bin/sh
# Builds the example plugins and lays each out as an installable folder in
# ./dist/<id>/ (plugin.toml + plugin.wasm). Copy a folder into the plugins
# folder (or use Plugins > Install) to try it.
set -eu
cd "$(dirname "$0")"
rustup target add wasm32-unknown-unknown >/dev/null 2>&1 || true
cargo build --release --target wasm32-unknown-unknown
for p in privacy-redirects quick-notes gh-omnibox; do
    mkdir -p "dist/$p"
    cp "$p/plugin.toml" "dist/$p/plugin.toml"
    cp "target/wasm32-unknown-unknown/release/$(echo "$p" | tr - _).wasm" "dist/$p/plugin.wasm"
done
ls -l dist/*/plugin.wasm
