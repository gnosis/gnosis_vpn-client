#!/usr/bin/env bash
# One-time setup: fetch the obscura headless-browser binary and install Node deps.
# Everything it downloads is gitignored. Re-run any time to refresh.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE"

OBSCURA_VER="latest"
case "$(uname -sm)" in
  "Darwin arm64")  ASSET="obscura-aarch64-macos.tar.gz" ;;
  "Linux x86_64")  ASSET="obscura-x86_64-linux.tar.gz" ;;
  *) echo "Unsupported platform: $(uname -sm). obscura ships macOS-arm64 and linux-x86_64 prebuilts only."; exit 1 ;;
esac

echo "== fetching obscura ($ASSET) =="
mkdir -p bin
curl -fsSL -o obscura.tar.gz \
  "https://github.com/h4ckf0r0day/obscura/releases/${OBSCURA_VER}/download/${ASSET}"
tar xzf obscura.tar.gz
mv -f obscura obscura-worker bin/ 2>/dev/null || mv -f obscura bin/
rm -f obscura.tar.gz
chmod +x bin/obscura bin/obscura-worker 2>/dev/null || true
# clear macOS Gatekeeper quarantine so the downloaded binary can run
xattr -dr com.apple.quarantine bin/obscura bin/obscura-worker 2>/dev/null || true
echo "   obscura $(./bin/obscura --version 2>/dev/null || echo '?')"

echo "== installing Node deps =="
if [ -f package-lock.json ]; then npm ci; else npm install; fi

echo "== done. Run: ./run_baseline.sh [EXIT ...]  (default: USA UK_1 India) =="
