#!/usr/bin/env bash
# Скачивает локальные модели для анализа и проверяет контрольные суммы.
# Кладёт их туда, где их ищет приложение: ~/.local/share/app.avtonarezka.desktop/models
# Общий размер около 3 ГБ. Скачивание можно прервать и запустить снова: оно продолжится.
set -u

ROOT="${XDG_DATA_HOME:-$HOME/.local/share}/app.avtonarezka.desktop/models"

fetch() { # папка  url  файл  размер_в_байтах  sha256
  local dir=$1 url=$2 file=$3 size=$4 sha=$5
  mkdir -p "$ROOT/$dir"
  if [ -f "$ROOT/$dir/$file" ] && [ "$(stat -c %s "$ROOT/$dir/$file")" = "$size" ]; then
    echo "[$dir] уже скачано"
    return 0
  fi
  echo "[$dir] скачиваю $file ($((size / 1000000)) МБ)"
  curl -L --fail --retry 4 --retry-delay 3 -C - -sS -o "$ROOT/$dir/$file.part" "$url" || { echo "[$dir] ОШИБКА загрузки"; return 1; }
  local got real
  got=$(stat -c %s "$ROOT/$dir/$file.part")
  [ "$got" = "$size" ] || { echo "[$dir] ОШИБКА: размер $got вместо $size"; return 1; }
  real=$(sha256sum "$ROOT/$dir/$file.part" | cut -d' ' -f1)
  [ "$real" = "$sha" ] || { echo "[$dir] ОШИБКА: контрольная сумма $real вместо $sha"; rm -f "$ROOT/$dir/$file.part"; return 1; }
  mv "$ROOT/$dir/$file.part" "$ROOT/$dir/$file"
  echo "[$dir] готово"
}

status=0
# Whisper large-v3-turbo (распознавание речи), квантование q5_0
fetch whisper-large-v3-turbo-q5_0 \
  "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin" \
  ggml-large-v3-turbo-q5_0.bin 574041195 394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2 || status=1
# YAMNet (смех, крики, аплодисменты)
fetch yamnet-onnx \
  "https://huggingface.co/zeropointnine/yamnet-onnx/resolve/main/yamnet.onnx" \
  yamnet.onnx 16093603 1510041dce24a2e9e84ec546807ac408ae496da6d1ed41bc3ccba649623f8e19 || status=1
fetch yamnet-onnx \
  "https://huggingface.co/zeropointnine/yamnet-onnx/resolve/main/yamnet_class_map.csv" \
  yamnet_class_map.csv 14096 cdf24d193e196d9e95912a2667051ae203e92a2ba09449218ccb40ef787c6df2 || status=1
# Qwen3-4B (заголовки, подписи, оценка моментов), квантование Q4_K_M
fetch qwen3-4b-q4_k_m \
  "https://huggingface.co/Qwen/Qwen3-4B-GGUF/resolve/main/Qwen3-4B-Q4_K_M.gguf" \
  Qwen3-4B-Q4_K_M.gguf 2497280256 7485fe6f11af29433bc51cab58009521f205840f5b4ae3a32fa7f92e8534fdf5 || status=1

[ "$status" = 0 ] && echo "Все модели на месте: $ROOT" || echo "Часть моделей не скачана, запусти скрипт ещё раз."
exit $status
