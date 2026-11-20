#!/usr/bin/env bash
# Проверка типов Windows-приложения и LLM-воркера на Linux без загрузок и упаковки.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WIN_TARGET=x86_64-pc-windows-msvc

fail() { printf '%s\n' "$*" >&2; exit 1; }
for tool in cargo-xwin rustc unshare clang lld-link llvm-lib; do
  command -v "$tool" >/dev/null || fail "Нужен уже установленный $tool. Этот скрипт ничего не устанавливает."
done

export XWIN_CACHE_DIR="${XWIN_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/cargo-xwin}"
[[ -f "$XWIN_CACHE_DIR/xwin/DONE" ]] || fail "Нет готового кэша Windows SDK: $XWIN_CACHE_DIR"
read -r cached_arches < "$XWIN_CACHE_DIR/xwin/DONE"
[[ " $cached_arches " == *" x86_64 "* ]] || fail "В кэше SDK нет x86_64."
[[ -f "$ROOT/windows/.cache/vulkan-sdk/Lib/vulkan.lib" ]] || fail "Нет подготовленной Vulkan import-библиотеки."

# Без сетевого namespace проверка не запускается. Cargo дополнительно запрещает
# сеть и изменение lock. Существующие SDK/зависимости переиспользуются.
exec unshare --user --map-root-user --net bash --noprofile --norc -c '
  set -euo pipefail
  ROOT="$1"
  . "$ROOT/windows/env.sh"
  # Даже rustc может быть rustup/mise-прокси с автоустановкой. Вызываем его
  # только после отключения сети, как и остальные программы toolchain.
  target_lib=$(rustc --print target-libdir --target "$WIN_TARGET")
  [[ -d "$target_lib" ]] || { printf "%s\n" "Rust-таргет $WIN_TARGET ещё не установлен." >&2; exit 1; }
  export CARGO_NET_OFFLINE=true
  export XWIN_HTTP_RETRIES=0
  export TAURI_CONFIG="$(cat "$ROOT/windows/tauri.windows.conf.json")"
  cd "$ROOT/src-tauri"
  exec cargo-xwin check --frozen --release --target "$WIN_TARGET" -j1 --workspace --lib --bins
' windows-check-offline "$ROOT"
