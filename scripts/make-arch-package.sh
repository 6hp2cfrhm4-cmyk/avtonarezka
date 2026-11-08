#!/usr/bin/env bash
# Собирает pacman-пакет из уже готового release-бинарника (сначала scripts/build-release.sh).
# Результат: release/avtonarezka-<версия>-x86_64.pkg.tar.zst
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$PWD

for f in src-tauri/target/release/avtonarezka src-tauri/target/release/llm-worker; do
  [ -x "$f" ] || { echo "Нет $f. Сначала scripts/build-release.sh"; exit 1; }
done
mkdir -p release
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cp packaging/arch/PKGBUILD "$work/"
(cd "$work" && PROJECT_DIR="$ROOT" PKGDEST="$ROOT/release" makepkg -f --noconfirm --nodeps)
ls -la release/*.pkg.tar.zst
