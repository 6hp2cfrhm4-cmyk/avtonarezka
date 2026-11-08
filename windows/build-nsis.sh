#!/usr/bin/env bash
# Собирает NSIS с 32-битными заглушками (x86) в windows/.cache/nsis. Нужен, потому что Tauri рассчитан на стандартный
# NSIS (x86-unicode, его плагин nsis_tauri_utils.dll 32-битный), а пакет nsis из AUR собирается с TARGET_ARCH=amd64 и
# делает только 64-битные установщики. 32-битный установщик ставит обычное 64-битное приложение, на Windows 11 это норма.
# Исходники: тот же архив, что берёт PKGBUILD (проверяется SHA256); из кэша yay или с SourceForge.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
OUT="$HERE/.cache/nsis"
VERSION=3.13
SHA=a8ffe024602d46b6d766f9e1ce30c324ad2a24daeacd3efc2642d436a0c157ac

if "$OUT/bin/makensis" -HDRINFO 2>/dev/null | grep -q 'NSIS_CPU=x86'; then
  echo "NSIS (x86) уже собран: $OUT"
  exit 0
fi
for t in scons i686-w64-mingw32-gcc; do
  command -v "$t" >/dev/null || { echo "Нужен $t: sudo pacman -S scons mingw-w64-gcc mingw-w64-zlib"; exit 1; }
done
[ -f /usr/i686-w64-mingw32/lib/libz.a ] || { echo "Нужен mingw-w64-zlib: sudo pacman -S mingw-w64-zlib"; exit 1; }

SRC="$HERE/.cache/downloads/nsis-$VERSION-src.tar.bz2"
mkdir -p "$HERE/.cache/downloads"
if [ ! -f "$SRC" ]; then
  if [ -f "$HOME/.cache/yay/nsis/nsis-$VERSION-src.tar.bz2" ]; then
    cp "$HOME/.cache/yay/nsis/nsis-$VERSION-src.tar.bz2" "$SRC"
  else
    curl -L --fail --retry 4 -sS -o "$SRC" "https://downloads.sourceforge.net/project/nsis/NSIS%203/$VERSION/nsis-$VERSION-src.tar.bz2"
  fi
fi
[ "$(sha256sum "$SRC" | cut -d' ' -f1)" = "$SHA" ] || { echo "Контрольная сумма архива NSIS не совпала" >&2; rm -f "$SRC"; exit 1; }

BUILD="$HERE/.cache/nsis-build"
rm -rf "$BUILD" "$OUT"
mkdir -p "$BUILD"
tar -xjf "$SRC" -C "$BUILD"
cd "$BUILD/nsis-$VERSION-src"
# SKIPUTILS и NSIS_MAX_STRLEN=8192 те же, что в PKGBUILD (скрипт установщика Tauri использует длинные строки)
# без -j: скрипты документации NSIS меняют текущий каталог, и параллельная сборка ломает относительные пути
scons VERSION=$VERSION PREFIX="$OUT" PREFIX_CONF="$OUT/etc" SKIPUTILS='NSIS Menu' STRIP_CP=false \
  TARGET_ARCH=x86 ZLIB_W32=/usr/i686-w64-mingw32/ NSIS_MAX_STRLEN=8192 NSIS_CONFIG_LOG=yes install
rm -rf "$BUILD"
"$OUT/bin/makensis" -VERSION
