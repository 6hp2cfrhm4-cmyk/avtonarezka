#!/usr/bin/env bash
# Сборка Автонарезки под Windows 11 с Linux: воркер языковой модели → приложение → установщик NSIS → windows/release/.
#   windows/build-windows.sh                 всё
#   SKIP_TOOLS=1 windows/build-windows.sh    не трогать windows/vendor/tools (ffmpeg, yt-dlp, deno уже лежат)
# Для запасного загрузчика Vulkan нужен git (исходники Khronos качаются один раз).
# Первый запуск долгий: качается кэш Windows SDK (~1,2 ГБ) и собираются whisper.cpp и llama.cpp.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

"$ROOT/windows/setup-toolchain.sh"
if [ -z "${SKIP_TOOLS:-}" ]; then
  "$ROOT/windows/fetch-tools.sh"
fi
# shellcheck source=windows/env.sh
. "$ROOT/windows/env.sh"

echo "== воркер языковой модели (Windows, Vulkan)"
(cd src-tauri && cargo xwin build --release --target "$WIN_TARGET" -p llm-worker)
# Tauri кладёт внешние бинарники рядом с программой; имя обязано содержать целевую платформу
mkdir -p src-tauri/binaries
cp "src-tauri/target/$WIN_TARGET/release/llm-worker.exe" "src-tauri/binaries/llm-worker-$WIN_TARGET.exe"

echo "== запасной загрузчик Vulkan (vk\\vulkan-1.dll)"
"$ROOT/windows/build-vulkan-loader.sh"

echo "== приложение и установщик NSIS"
npx tauri build --runner cargo-xwin --target "$WIN_TARGET" --config windows/tauri.windows.conf.json --bundles nsis

echo "== windows/release"
rm -rf windows/release
mkdir -p windows/release
cp src-tauri/target/"$WIN_TARGET"/release/bundle/nsis/*-setup.exe windows/release/
cp windows/fetch-models.ps1 windows/release/
cp windows/ИНСТРУКЦИЯ.txt windows/release/
(cd windows/release && sha256sum -- *.exe *.ps1 > SHA256SUMS)
echo "Готово. Файлы в windows/release/:"
ls -1sh windows/release/ | sed 's/^/  /'
