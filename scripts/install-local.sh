#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
install_dir="${STRATA_INSTALL_DIR:-$HOME/.local/bin}"
binary_name="strata"

mkdir -p "$install_dir"

echo "Building $binary_name in release mode..."
cargo build --release --manifest-path "$repo_root/Cargo.toml"

echo "Installing to $install_dir/$binary_name"
install -m 0755 "$repo_root/target/release/$binary_name" "$install_dir/$binary_name"

echo "Installed $binary_name"
echo "Run: $binary_name --help"
