#!/usr/bin/env bash
# Релизная сборка: воркер языковой модели → приложение и пакеты Tauri → пакет для Arch.
#   scripts/build-release.sh                 всё
#   BUNDLES=deb scripts/build-release.sh     только указанные форматы Tauri (deb, appimage, rpm)
#   SKIP_ARCH=1 scripts/build-release.sh     без пакета для Arch
set -euo pipefail
cd "$(dirname "$0")/.."

TRIPLE=$(rustc -vV | sed -n 's/^host: //p')

echo "== воркер языковой модели (release, Vulkan)"
(cd src-tauri && cargo build --release -p llm-worker)
# Tauri кладёт внешние бинарники в пакет рядом с программой; имя обязано содержать целевую платформу
mkdir -p src-tauri/binaries
cp src-tauri/target/release/llm-worker "src-tauri/binaries/llm-worker-$TRIPLE"

echo "== приложение и пакеты Tauri"
# NO_STRIP: упаковщик AppImage на Arch не понимает современные секции ELF, а бинарники и так уже stripped
NO_STRIP=true npx tauri build ${BUNDLES:+--bundles $BUNDLES}

if [ -z "${SKIP_ARCH:-}" ]; then
  echo "== пакет для Arch"
  scripts/make-arch-package.sh
fi
# все готовые пакеты — в одну папку, с контрольными суммами
mkdir -p release
cp -f src-tauri/target/release/bundle/deb/*.deb src-tauri/target/release/bundle/appimage/*.AppImage release/ 2>/dev/null || true
(cd release && sha256sum -- *.deb *.AppImage *.pkg.tar.zst 2>/dev/null > SHA256SUMS || true)
echo "Готово. Пакеты в release/:"
ls -1sh release/ | sed 's/^/  /'
