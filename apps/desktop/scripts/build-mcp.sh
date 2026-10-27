#!/usr/bin/env bash
# Builds nanace-mcp and places it where Tauri expects the sidecar:
# src-tauri/binaries/nanace-mcp-<target triple>.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
root="$(cd "$here/../.." && pwd)"
triple="$(rustc -vV | sed -n 's/^host: //p')"
ext=""
case "$triple" in *windows*) ext=".exe" ;; esac
cargo build --release -p nanace-mcp --manifest-path "$root/Cargo.toml"
mkdir -p "$here/src-tauri/binaries"
cp "$root/target/release/nanace-mcp$ext" "$here/src-tauri/binaries/nanace-mcp-$triple$ext"
