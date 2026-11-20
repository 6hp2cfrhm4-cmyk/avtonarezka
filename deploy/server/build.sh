#!/usr/bin/env bash
# Сборка avtonarezka-daemon и avtoctl под Ubuntu 24.04 в контейнере.
# Ничего не трогает на сервере. Первая сборка скачивает образ ubuntu:24.04, пакеты и Rust (~0,5-1 ГБ),
# поэтому без явного согласия не запускается: передайте --yes.
set -euo pipefail
cd "$(dirname "$0")/../.."
OUT="${OUT:-deploy/server/dist}"
IMG="avtonarezka-build:ubuntu2404"
if [ "${1:-}" != "--yes" ]; then
  echo "Будет скачано: образ ubuntu:24.04, пакеты apt и тулчейн Rust (порядка 0,5–1 ГБ)."
  echo "Запустите с --yes, если вы согласны."
  exit 2
fi
command -v docker >/dev/null || { echo "нужен docker"; exit 1; }
mkdir -p "$OUT" .cache/apt-ubuntu2404 .cache/cargo-ubuntu2404 .cache/target-ubuntu2404 .cache/rustup-ubuntu2404
# Всё в одном контейнере с сетью хоста и DNS хоста (в сборочном контейнере docker DNS не раздаёт).
docker run --rm --network host -v /run/systemd/resolve/stub-resolv.conf:/etc/resolv.conf:ro \
  -e CARGO_TARGET_DIR=/cache/target -e CARGO_HOME=/cache/cargo -e RUSTUP_HOME=/cache/rustup \
  -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
  -v "$PWD":/src -v "$PWD/.cache/cargo-ubuntu2404":/cache/cargo -v "$PWD/.cache/target-ubuntu2404":/cache/target \
  -v "$PWD/.cache/rustup-ubuntu2404":/cache/rustup -v "$PWD/.cache/apt-ubuntu2404":/var/cache/apt/archives -w /src/src-tauri ubuntu:24.04 bash -c '
    set -e
    export DEBIAN_FRONTEND=noninteractive PATH=/cache/cargo/bin:$PATH
    apt-get -o Acquire::Retries=5 -o Acquire::http::Timeout=30 update -qq && apt-get -o Acquire::Retries=5 -o Acquire::http::Timeout=30 install -y -qq --no-install-recommends build-essential pkg-config ca-certificates curl cmake clang libssl-dev >/dev/null
    [ -x /cache/cargo/bin/cargo ] || { curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable; }
    cargo build --release --no-default-features --features headless --bin avtonarezka-daemon --bin avtoctl
    chown -R "$HOST_UID:$HOST_GID" /cache/target'
cp .cache/target-ubuntu2404/release/avtonarezka-daemon .cache/target-ubuntu2404/release/avtoctl "$OUT/"
( cd "$OUT" && sha256sum avtonarezka-daemon avtoctl > SHA256SUMS )
for b in avtonarezka-daemon avtoctl; do
  v=$(objdump -T "$OUT/$b" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)
  echo "$b требует $v (на сервере 2.39)"
done
