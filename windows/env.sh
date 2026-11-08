# Переменные окружения кросс-сборки под Windows. Подключается через `. windows/env.sh`.
# Нужен $ROOT — корень проекта (его задаёт вызывающий скрипт).

export WIN_TARGET=x86_64-pc-windows-msvc
# NSIS с 32-битными заглушками, собранный windows/build-nsis.sh (nsis из AUR умеет только 64-битные)
export PATH="$ROOT/windows/.cache/nsis/bin:$PATH"
# «SDK» Vulkan из заголовков системы и import-библиотек, которые делает setup-toolchain.sh
export VULKAN_SDK="$ROOT/windows/.cache/vulkan-sdk"
# whisper-rs-sys из-за проверки ОС хоста не добавляет путь поиска vulkan.lib; lld-link читает его из LIB
# (не RUSTFLAGS: смена флагов заставила бы пересобрать все зависимости)
export LIB="$VULKAN_SDK/Lib"
# Статический CRT: на чистой Windows 11 нет VC++ Redistributable, и без этого программа не запустится.
# Rust линкует CRT статически, C++-части (ggml, SQLite) собираются с /MT
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C target-feature=+crt-static"
export LLAMA_STATIC_CRT=1
# whisper.cpp просит CMake 3.5, а там политика CMP0091 старая и CMAKE_MSVC_RUNTIME_LIBRARY молча игнорируется
# (получается /MD, и линковка падает на __declspec(dllimport)). whisper-rs-sys передаёт CMAKE_* из окружения как есть
export CMAKE_POLICY_DEFAULT_CMP0091=NEW
export CMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded
# cargo-xwin сам скачивает MSVC CRT и Windows SDK в ~/.cache/cargo-xwin (принятие лицензии Microsoft)
export XWIN_ARCH=x86_64
