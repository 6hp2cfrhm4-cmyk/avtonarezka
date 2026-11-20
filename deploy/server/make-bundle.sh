#!/usr/bin/env bash
# Собирает набор для установки на чужой сервер: бинарники (после build.sh), скрипты, юниты.
#   deploy/server/make-bundle.sh [версия]  →  deploy/server/dist/avtonarezka-server-<версия>.tar.gz (+ .sha256)
set -euo pipefail
cd "$(dirname "$0")"
VER="${1:-$(sed -n 's/^version = "\(.*\)"/\1/p' ../../src-tauri/Cargo.toml | head -1)}"
for f in dist/avtonarezka-daemon dist/avtoctl; do [ -x "$f" ] || { echo "нет $f: сначала ./build.sh --yes"; exit 1; }; done
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
D="$T/avtonarezka-server"; mkdir -p "$D/dist" "$D/units"
cp dist/avtonarezka-daemon dist/avtoctl "$D/dist/"
cp install.sh uninstall.sh lib.sh preflight.sh postflight.sh loadtest.sh fetch-tools.sh avtoctl-gate sudoers.avtonarezka "$D/"
cp units/* "$D/units/"
cp ../../docs/SERVER_SETUP.md "$D/README.md"
( cd "$D/dist" && sha256sum avtonarezka-daemon avtoctl > SHA256SUMS )
chmod +x "$D"/*.sh
OUT="dist/avtonarezka-server-$VER.tar.gz"
tar -C "$T" -czf "$OUT" avtonarezka-server
( cd dist && sha256sum "avtonarezka-server-$VER.tar.gz" > "avtonarezka-server-$VER.tar.gz.sha256" )
echo "$OUT"; ls -la "$OUT"
