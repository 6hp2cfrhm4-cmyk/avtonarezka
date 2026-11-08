#!/usr/bin/env bash
# Готовит кросс-сборку под Windows: проверяет программы, ставит Rust-таргет и cargo-xwin (в ~/.cargo, без sudo)
# и собирает «заглушку» Vulkan SDK в windows/.cache/vulkan-sdk. Системные пакеты сам не ставит, только перечисляет.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
TARGET=x86_64-pc-windows-msvc

missing=()
need() { command -v "$1" >/dev/null 2>&1 || missing+=("$1 ($2)"); }
need clang-cl "пакет clang"
need lld-link "пакет lld"
need llvm-lib "пакет llvm"
need llvm-dlltool "пакет llvm"
need llvm-rc "пакет llvm"
need cmake "пакет cmake"
need ninja "пакет ninja"
need glslc "пакет shaderc"
need unzip "пакет unzip"
need curl "пакет curl"
need node "Node.js"
need npm "Node.js"
if [ ${#missing[@]} -gt 0 ]; then
  echo "Не хватает программ:"
  printf '  %s\n' "${missing[@]}"
  echo
  echo "Поставь так (Arch):"
  echo "  sudo pacman -S lld ninja nasm wine      # wine нужен только для проверок windows/smoke-wine.sh"
  echo "  sudo pacman -S scons mingw-w64-gcc mingw-w64-zlib   # для сборки NSIS (windows/build-nsis.sh)"
  exit 1
fi
"$HERE/build-nsis.sh"
for h in /usr/include/vulkan/vulkan.hpp /usr/include/spirv/unified1/spirv.hpp; do
  [ -f "$h" ] || { echo "Нет $h. Нужны пакеты vulkan-headers и spirv-headers (sudo pacman -S vulkan-headers spirv-headers)."; exit 1; }
done

if ! rustup target list --installed | grep -qx "$TARGET"; then
  echo "== Rust-таргет $TARGET"
  rustup target add "$TARGET"
fi
if ! command -v cargo-xwin >/dev/null 2>&1; then
  echo "== cargo-xwin"
  cargo install cargo-xwin --locked
fi

# Vulkan SDK для Windows-цели из того, что есть: заголовки хоста, import-библиотека из списка экспорта загрузчика
# (windows/vulkan/vulkan-1.def, Khronos, Apache-2.0), glslc хоста. Саму vulkan-1.dll ставит драйвер видеокарты.
SDK="$HERE/.cache/vulkan-sdk"
mkdir -p "$SDK/Include" "$SDK/Lib" "$SDK/Bin"
ln -sfn /usr/include/vulkan "$SDK/Include/vulkan"
ln -sfn /usr/include/vk_video "$SDK/Include/vk_video"
# clang-cl не ищет в /usr/include, а ggml-vulkan подключает <spirv/unified1/spirv.hpp>: кладём заголовки рядом с Vulkan
ln -sfn /usr/include/spirv "$SDK/Include/spirv"
ln -sfn "$(command -v glslc)" "$SDK/Bin/glslc"
llvm-dlltool -m i386:x86-64 -d "$HERE/vulkan/vulkan-1.def" -D vulkan-1.dll -l "$SDK/Lib/vulkan-1.lib"
# whisper-rs-sys 0.15 проверяет ОС хоста (cfg!(windows) в build.rs), а не цели, и на Linux просит `-lvulkan`.
# Вторая библиотека с тем же содержимым (ссылается на vulkan-1.dll) закрывает это без правки крейта.
llvm-dlltool -m i386:x86-64 -d "$HERE/vulkan/vulkan-1.def" -D vulkan-1.dll -l "$SDK/Lib/vulkan.lib"
# Rust-часть сборки (build.rs llama.cpp и whisper.cpp) ищет Lib/Bin с большой буквы, как в SDK для Windows,
# а CMake (FindVulkan) на Linux-хосте смотрит include/lib/bin с маленькой: даём обе записи.
for d in Include Lib Bin; do ln -sfn "$d" "$SDK/$(echo "$d" | tr 'A-Z' 'a-z')"; done
echo "Окружение готово: Rust-таргет, cargo-xwin, Vulkan SDK в $SDK"
