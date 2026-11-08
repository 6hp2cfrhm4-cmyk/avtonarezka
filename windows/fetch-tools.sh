#!/usr/bin/env bash
# Скачивает утилиты, которые едут в установщик Windows (windows/vendor/tools): ffmpeg, ffprobe, yt-dlp, deno.
# Версии и SHA256 закреплены ниже: скачанное сверяется, при несовпадении файл удаляется и сборка останавливается.
# Обновить утилиту: поменять версию и сумму здесь (суммы публикуют сами авторы: у GitHub-релизов есть digest).
set -euo pipefail
cd "$(dirname "$0")"
OUT=vendor/tools
DL=.cache/downloads
mkdir -p "$OUT" "$DL"

# ffmpeg 9.0 (как на Linux у разработчика): сборка BtbN с GPL-кодеками, NVENC (h264_nvenc) и libass для субтитров
FFMPEG_TAG=autobuild-2026-11-07-12-47
FFMPEG_ZIP=ffmpeg-n9.0.2-48-gd4070a14ce-win64-gpl-9.0.zip
FFMPEG_SHA=b33f1e3383f97c7acc8502e475df2faabcaee7d9605facda9b213cfbb39e0300
# yt-dlp: версия не старее той, на которую рассчитан скачиватель (MIN_YTDLP в src/lib/youtube.ts)
YTDLP_VERSION=2026.10.26
YTDLP_SHA=f5389d3698ecde4ef06f4e6b1ce0294fe9ac1396228c8990b8d3416d6d177a7d
# Deno: JS-рантайм, без которого YouTube отдаёт неполный список форматов
DENO_VERSION=2.9.8
DENO_SHA=cba84b26e0df5ae7f74fc66b86a95b866872a86292ebd84fa1e959927edd93bf

fetch() { # url файл sha256
  local url=$1 file=$DL/$2 sha=$3
  if [ -f "$file" ] && [ "$(sha256sum "$file" | cut -d' ' -f1)" = "$sha" ]; then
    echo "[$2] уже скачан"
    return 0
  fi
  echo "[$2] скачиваю"
  curl -L --fail --retry 4 --retry-delay 3 -sS -o "$file.part" "$url"
  local real
  real=$(sha256sum "$file.part" | cut -d' ' -f1)
  if [ "$real" != "$sha" ]; then
    rm -f "$file.part"
    echo "[$2] ОШИБКА: контрольная сумма $real вместо $sha" >&2
    return 1
  fi
  mv "$file.part" "$file"
}

fetch "https://github.com/BtbN/FFmpeg-Builds/releases/download/$FFMPEG_TAG/$FFMPEG_ZIP" "$FFMPEG_ZIP" "$FFMPEG_SHA"
fetch "https://github.com/yt-dlp/yt-dlp/releases/download/$YTDLP_VERSION/yt-dlp.exe" "yt-dlp-$YTDLP_VERSION.exe" "$YTDLP_SHA"
fetch "https://github.com/denoland/deno/releases/download/v$DENO_VERSION/deno-x86_64-pc-windows-msvc.zip" "deno-$DENO_VERSION.zip" "$DENO_SHA"

rm -f "$OUT"/*.exe "$OUT"/*.txt
unzip -j -o -q "$DL/$FFMPEG_ZIP" '*/bin/ffmpeg.exe' '*/bin/ffprobe.exe' '*/LICENSE.txt' -d "$OUT"
mv "$OUT/LICENSE.txt" "$OUT/LICENSE-ffmpeg.txt"
cp "$DL/yt-dlp-$YTDLP_VERSION.exe" "$OUT/yt-dlp.exe"
unzip -o -q "$DL/deno-$DENO_VERSION.zip" deno.exe -d "$OUT"

for f in ffmpeg ffprobe yt-dlp deno; do
  [ -s "$OUT/$f.exe" ] || { echo "Нет $OUT/$f.exe" >&2; exit 1; }
  head -c2 "$OUT/$f.exe" | grep -q MZ || { echo "$OUT/$f.exe не похож на программу Windows" >&2; exit 1; }
done

cat > "$OUT/README-tools.txt" <<NOTE
Эти программы запускаются Автонарезкой как отдельные процессы; они не часть приложения и имеют свои лицензии.

ffmpeg, ffprobe   $FFMPEG_ZIP (BtbN, GPL). Лицензия: LICENSE-ffmpeg.txt.
                  Исходники и сборочные скрипты: https://github.com/BtbN/FFmpeg-Builds и https://ffmpeg.org/download.html
yt-dlp.exe        $YTDLP_VERSION (Unlicense). https://github.com/yt-dlp/yt-dlp
                  Обновить: yt-dlp.exe -U (YouTube меняется часто, на старой версии бывают ошибки).
deno.exe          $DENO_VERSION (MIT). https://github.com/denoland/deno
NOTE
echo "Готово: $OUT"
ls -la "$OUT"
