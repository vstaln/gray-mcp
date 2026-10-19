#!/bin/sh
here="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
sys="$(uname -s)"
case "$sys" in
MINGW* | MSYS* | CYGWIN*) rel="x86_64-pc-windows-msvc/gray-mcp.exe" ;;
Darwin*)
    case "$(uname -m)" in
    arm64) rel="aarch64-apple-darwin/gray-mcp" ;;
    *) rel="x86_64-apple-darwin/gray-mcp" ;;
    esac ;;
*)
    case "$(uname -m)" in
    aarch64 | arm64) rel="aarch64-unknown-linux-musl/gray-mcp" ;;
    *) rel="x86_64-unknown-linux-musl/gray-mcp" ;;
    esac ;;
esac
if [ -x "$here/$rel" ]; then exec "$here/$rel" "$@"; fi
for cand in "$here/target/release/gray-mcp.exe" "$here/target/release/gray-mcp" "$here/target/debug/gray-mcp.exe" "$here/target/debug/gray-mcp"; do
    if [ -x "$cand" ]; then exec "$cand" "$@"; fi
done
echo "gray-mcp binary not found in $here" >&2
exit 1
