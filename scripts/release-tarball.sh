#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
target=x86_64-unknown-linux-musl
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
bin="target/$target/release/gray-mcp"
CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=cc CARGO_BUILD_BUILD_DIR=/target \
  cross build --release --locked --target "$target"
file -b "$bin" | grep -q 'static-pie linked' || {
  echo "gray-mcp: not a static binary: $(file -b "$bin")" >&2
  exit 1
}
"$bin" manifest >/dev/null
mkdir -p dist
tar czf "dist/gray-mcp-$version.tar.gz" -C "target/$target/release" gray-mcp
sha256sum "dist/gray-mcp-$version.tar.gz"
