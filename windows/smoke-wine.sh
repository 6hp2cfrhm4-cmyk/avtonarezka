#!/usr/bin/env bash
# Дымовые проверки Windows-утилит под Wine (нужен пакет wine): запускаются ли они и верно ли ffmpeg разбирает
# Windows-пути с пробелом и кириллицей в фильтре субтитров (то же экранирование, что делает render/filter.rs).
# Окно самого приложения так не проверить: WebView2 под Wine ненадёжен, его смотри на настоящей Windows.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
TOOLS="$ROOT/windows/vendor/tools"
WORKER="$ROOT/src-tauri/target/x86_64-pc-windows-msvc/release/llm-worker.exe"
command -v wine >/dev/null || { echo "Нужен wine: sudo pacman -S wine"; exit 1; }

export WINEPREFIX="${WINEPREFIX:-$ROOT/windows/.cache/wineprefix}"
export WINEDEBUG=-all WINEARCH=win64 WINEDLLOVERRIDES="mscoree,mshtml="   # без запроса на установку Mono и Gecko
wine wineboot -i >/dev/null 2>&1 || true

pass=0; fail=0
check() { # название команда...
  local name=$1; shift
  if "$@" >/dev/null 2>&1; then echo "  ok   $name"; pass=$((pass+1)); else echo "  FAIL $name"; fail=$((fail+1)); fi
}

echo "== ffmpeg"
check "ffmpeg -version"        wine "$TOOLS/ffmpeg.exe" -version
check "ffprobe -version"       wine "$TOOLS/ffprobe.exe" -version
check "h264_nvenc в списке"    bash -c "wine '$TOOLS/ffmpeg.exe' -hide_banner -encoders 2>/dev/null | grep -q h264_nvenc"
check "фильтр ass (libass)"    bash -c "wine '$TOOLS/ffmpeg.exe' -hide_banner -filters 2>/dev/null | grep -q ' ass '"

echo "== субтитры по Windows-пути с пробелом и кириллицей"
DIR="$WINEPREFIX/drive_c/Пробный каталог"
rm -rf "$DIR"; mkdir -p "$DIR/fonts"
cp "$ROOT"/assets/fonts/*.ttf "$DIR/fonts/"
cat > "$DIR/job.ass" <<'ASS'
[Script Info]
ScriptType: v4.00+
PlayResX: 320
PlayResY: 240

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Montserrat ExtraBold,28,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,0,2,10,10,10,1

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,Привет, Windows
ASS
# C:/Пробный каталог/job.ass после двух уровней экранирования: C\\:/Пробный каталог/job.ass
check "ass=filename=C\\\\:/… + fontsdir" wine "$TOOLS/ffmpeg.exe" -y -v error \
  -f lavfi -i "color=c=0x202040:s=320x240:d=1:r=25" \
  -vf "ass=filename=C\\\\:/Пробный каталог/job.ass:fontsdir=C\\\\:/Пробный каталог/fonts,format=yuv420p" \
  -c:v libx264 -preset ultrafast "C:\\Пробный каталог\\out.mp4"
check "файл с кириллицей создан" test -s "$DIR/out.mp4"

echo "== llm-worker"
if [ -f "$WORKER" ]; then
  # без модели воркер обязан запуститься и внятно сообщить, что ему нужен --model
  check "llm-worker.exe запускается" bash -c "wine '$WORKER' 2>&1 | grep -q -- '--model'"
else
  echo "  --   нет $WORKER (сначала windows/build-windows.sh)"
fi

echo "== загрузчик Vulkan у воркера (с настоящей моделью Qwen3-4B, около минуты на режим)"
MODEL="$HOME/.local/share/app.avtonarezka.desktop/models/qwen3-4b-q4_k_m/Qwen3-4B-Q4_K_M.gguf"
VKDLL="$ROOT/windows/vendor/vulkan/vulkan-1.dll"
if [ -f "$WORKER" ] && [ -f "$MODEL" ] && [ -f "$VKDLL" ]; then
  TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
  mkdir -p "$TMP/vk"; cp "$WORKER" "$TMP/"; cp "$VKDLL" "$TMP/vk/"   # так лежит в установке: воркер, рядом vk\vulkan-1.dll
  WMODEL="Z:${MODEL//\//\\}"
  REQ='{"id":1,"prompt":"<|im_start|>user\nСкажи одно слово: привет<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n","maxTokens":6,"temperature":0}'
  ask_worker() { # переменные окружения для wine...: всё, что воркер сказал в stdout и stderr
    (cd "$TMP" && { printf '%s\n' "$REQ"; sleep 1; } | env "$@" timeout 280 wine llm-worker.exe --model "$WMODEL" --gpu-layers 99 --threads 8 --ctx 1024 2>&1) || true
  }
  has() { grep -q -- "$1" <<<"$out"; }
  # В Wine системный vulkan-1.dll есть: берётся он, а запасной не нужен
  system_loader() { has '"text":"Привет' && ! has 'запасной загрузчик'; }
  out=$(ask_worker WINEDLLOVERRIDES="mscoree,mshtml=")
  check "системный загрузчик: воркер отвечает, запасной не нужен" system_loader
  # Системный запрещён (vulkan-1=n), драйверов нет (VK_DRIVER_FILES в пустоту): берётся vk\vulkan-1.dll, видеокарт нет, процессор
  bundled_loader() { has '"text":"Привет' && has 'взят запасной загрузчик' && has 'видеокарт нет'; }
  out=$(ask_worker WINEDLLOVERRIDES="mscoree,mshtml=;vulkan-1=n" VK_DRIVER_FILES='C:\nonexistent\none.json')
  check "без системного загрузчика и драйверов: запасной, воркер на процессоре" bundled_loader
else
  echo "  --   нужны воркер, vulkan-1.dll (windows/build-vulkan-loader.sh) и модель $MODEL"
fi

echo; echo "Итого: ok=$pass, ошибок=$fail"
[ "$fail" = 0 ]
