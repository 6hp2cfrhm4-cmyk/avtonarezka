#!/usr/bin/env bash
# Скачивает внешние программы и модели для сервера в каталоги ./tools и ./models и проверяет контрольные суммы.
# Сам ничего не устанавливает: потом `sudo ./install.sh --step tools --tools-dir tools` и `--step models --models-dir models`.
# Размер: ffmpeg ≈ 42 МБ, yt-dlp ≈ 3 МБ, Deno ≈ 42 МБ, Whisper small ≈ 190 МБ, YAMNet ≈ 16 МБ.
# Источники: johnvansickle.com (ffmpeg), github.com/yt-dlp, github.com/denoland, huggingface.co (модели).
#   ./fetch-tools.sh --yes [--no-models] [--no-deno]
set -euo pipefail
cd "$(dirname "$0")"
YES=0 MODELS=1 DENO=1
for a in "$@"; do case "$a" in --yes) YES=1 ;; --no-models) MODELS=0 ;; --no-deno) DENO=0 ;; *) echo "неизвестный параметр: $a"; exit 2 ;; esac; done
[ "$YES" = 1 ] || { sed -n 2,5p "$0"; echo; echo "Запустите с --yes, если согласны на скачивание."; exit 2; }
for c in curl tar sha256sum md5sum python3; do command -v "$c" >/dev/null || { echo "нужна утилита $c"; exit 1; }; done
[ "$(uname -m)" = x86_64 ] || { echo "сборки рассчитаны на x86_64"; exit 1; }
mkdir -p tools models
W="$(mktemp -d)"; trap 'rm -rf "$W"' EXIT
dl() { curl -fL --retry 3 -sS -o "$1" "$2"; }

echo "[ffmpeg] скачиваю"
dl "$W/ffmpeg.tar.xz" https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz
dl "$W/ffmpeg.md5" https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz.md5
[ "$(md5sum "$W/ffmpeg.tar.xz" | cut -d' ' -f1)" = "$(cut -d' ' -f1 "$W/ffmpeg.md5")" ] || { echo "ffmpeg: контрольная сумма не сошлась"; exit 1; }
mkdir "$W/ff" && tar -xJf "$W/ffmpeg.tar.xz" -C "$W/ff" --strip-components=1
install -m 0755 "$W/ff/ffmpeg" "$W/ff/ffprobe" tools/

echo "[yt-dlp] скачиваю"
dl "$W/yt-dlp" https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp
dl "$W/yt.sums" https://github.com/yt-dlp/yt-dlp/releases/latest/download/SHA2-256SUMS
want=$(grep -E ' yt-dlp$' "$W/yt.sums" | cut -d' ' -f1)
[ -n "$want" ] && [ "$(sha256sum "$W/yt-dlp" | cut -d' ' -f1)" = "$want" ] || { echo "yt-dlp: контрольная сумма не сошлась"; exit 1; }
install -m 0755 "$W/yt-dlp" tools/yt-dlp   # zipapp: ему нужен python3 на сервере

if [ "$DENO" = 1 ]; then
  echo "[deno] скачиваю (нужен yt-dlp для YouTube)"
  dl "$W/deno.zip" https://github.com/denoland/deno/releases/latest/download/deno-x86_64-unknown-linux-gnu.zip
  dl "$W/deno.sha" https://github.com/denoland/deno/releases/latest/download/deno-x86_64-unknown-linux-gnu.zip.sha256sum
  [ "$(sha256sum "$W/deno.zip" | cut -d' ' -f1)" = "$(cut -d' ' -f1 "$W/deno.sha")" ] || { echo "deno: контрольная сумма не сошлась"; exit 1; }
  python3 -c "import zipfile,sys; zipfile.ZipFile(sys.argv[1]).extract('deno', sys.argv[2])" "$W/deno.zip" "$W"
  install -m 0755 "$W/deno" tools/deno
fi
( cd tools && sha256sum * | grep -v SHA256SUMS > SHA256SUMS )

if [ "$MODELS" = 1 ]; then
  hf() { # папка файл размер sha256 url
    mkdir -p "models/$1"; echo "[модель] $2"
    dl "$W/m" "$5"; [ "$(sha256sum "$W/m" | cut -d' ' -f1)" = "$4" ] || { echo "$2: контрольная сумма не сошлась"; exit 1; }
    mv "$W/m" "models/$1/$2"
  }
  hf whisper-small-q5_1 ggml-small-q5_1.bin 190085487 ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small-q5_1.bin
  hf yamnet-onnx yamnet.onnx 16093603 1510041dce24a2e9e84ec546807ac408ae496da6d1ed41bc3ccba649623f8e19 https://huggingface.co/zeropointnine/yamnet-onnx/resolve/main/yamnet.onnx
  hf yamnet-onnx yamnet_class_map.csv 14096 cdf24d193e196d9e95912a2667051ae203e92a2ba09449218ccb40ef787c6df2 https://huggingface.co/zeropointnine/yamnet-onnx/resolve/main/yamnet_class_map.csv
  ( cd models && sha256sum */* > SHA256SUMS )
fi
echo "Готово. Дальше:"
echo "  sudo ./install.sh --step tools --tools-dir tools --yes"
[ "$MODELS" = 1 ] && echo "  sudo ./install.sh --step models --models-dir models --yes"
echo "  sudo systemctl restart avtonarezka-daemon"
