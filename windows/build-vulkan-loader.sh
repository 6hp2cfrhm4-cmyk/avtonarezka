#!/usr/bin/env bash
# Собирает загрузчик Vulkan для Windows (vulkan-1.dll) из исходников Khronos и кладёт в windows/vendor/vulkan/.
# Зачем он нужен рядом с программой: vulkan-1.dll обычно приносит драйвер видеокарты. Если драйвера нет (чистая Windows,
# виртуалка, удалённый рабочий стол), программа с системным загрузчиком не стартует вовсе. С нашим загрузчиком она
# запускается, видит ноль видеоускорителей, и Whisper с языковой моделью работают на процессоре. Если драйвер есть,
# наш загрузчик находит его по реестру так же, как системный.
#   windows/build-vulkan-loader.sh           соберёт, если ещё нет или сменилась версия
#   FORCE=1 windows/build-vulkan-loader.sh   пересобрать
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)

# Версия загрузчика = версия заголовков Vulkan в системе (vulkan-headers). Коммит проверяем, чтобы тег нельзя было подменить
TAG=v1.4.363
COMMIT=9e9518d2cf00054a220ae1a40f4406804b420ef5
URL=https://github.com/KhronosGroup/Vulkan-Loader

# Исходники и сборка лежат вне проекта, в каталоге с ASCII-путём: компилятор ресурсов llvm-rc падает на кириллице в путях
# (они попадают в строки препроцессора). Готовая vulkan-1.dll копируется в проект.
WORK="${XDG_CACHE_HOME:-$HOME/.cache}/avtonarezka-windows/vulkan-loader"
SRC="$WORK/src"
BUILD="$WORK/build"
HEADERS="$WORK/headers"
OUT="$HERE/vendor/vulkan"
STAMP="$HERE/vendor/.vulkan-loader.version"
MARK="$TAG $COMMIT"

if [ -z "${FORCE:-}" ] && [ -f "$OUT/vulkan-1.dll" ] && [ "$(cat "$STAMP" 2>/dev/null)" = "$MARK" ]; then
  echo "== загрузчик Vulkan $TAG уже собран ($OUT/vulkan-1.dll)"
  exit 0
fi

for c in git cmake ninja clang-cl lld-link llvm-rc llvm-lib llvm-objdump; do
  command -v "$c" >/dev/null 2>&1 || { echo "Нет программы $c (пакеты clang, lld, llvm, cmake, ninja)."; exit 1; }
done
XWIN_TOOLCHAIN="$HOME/.cache/cargo-xwin/cmake/clang-cl/x86_64-pc-windows-msvc-toolchain.cmake"
if [ ! -f "$XWIN_TOOLCHAIN" ]; then
  echo "Нет $XWIN_TOOLCHAIN: его создаёт первая сборка cargo xwin (windows/build-windows.sh или cargo xwin build)."
  exit 1
fi
[ -f /usr/share/cmake/VulkanHeaders/VulkanHeadersConfig.cmake ] || { echo "Нужен пакет vulkan-headers."; exit 1; }

echo "== исходники загрузчика Vulkan $TAG"
if [ "$(git -C "$SRC" rev-parse HEAD 2>/dev/null || true)" != "$COMMIT" ]; then
  rm -rf "$SRC"
  git clone --quiet --depth 1 --branch "$TAG" "$URL" "$SRC"
  got=$(git -C "$SRC" rev-parse HEAD)
  [ "$got" = "$COMMIT" ] || { rm -rf "$SRC"; echo "Коммит $got вместо $COMMIT: тег $TAG указывает не туда. Стоп."; exit 1; }
fi

# Две строки в CMake загрузчика (не в его коде) написаны под clang/gcc: при кросс-сборке он просит ассемблерный вывод флагом -S,
# который clang-cl игнорирует (получается объектный файл, и разбор падает). Для clang-cl то же самое делают
# /clang:-S и /Fa<файл>. Тег проверен выше по коммиту, поэтому если строк нет, это не наш тег, и лучше остановиться.
python3 - "$SRC/loader/CMakeLists.txt" <<'PY'
import sys
p = sys.argv[1]
s = open(p, encoding='utf-8').read()
subs = [
    ('target_compile_options(asm_offset PRIVATE -S -fno-lto -fno-whole-program-vtables -fno-virtual-function-elimination)',
     'target_compile_options(asm_offset PRIVATE /clang:-S "/Fa${CMAKE_CURRENT_BINARY_DIR}/asm_offset.asm" -fno-lto -fno-whole-program-vtables -fno-virtual-function-elimination)'),
    ('set(ASM_OFFSET_INTERMEDIATE_LOCATION "$<TARGET_OBJECTS:asm_offset>")',
     'set(ASM_OFFSET_INTERMEDIATE_LOCATION "${CMAKE_CURRENT_BINARY_DIR}/asm_offset.asm")'),
]
for old, new in subs:
    if new in s:
        continue
    if old not in s:
        sys.exit('В loader/CMakeLists.txt нет ожидаемой строки: ' + old)
    s = s.replace(old, new)
open(p, 'w', encoding='utf-8').write(s)
PY

# Заголовки Vulkan хоста лежат в /usr/include, а этот каталог нельзя отдавать clang-cl: рядом лежат заголовки glibc,
# и они перебивают MSVC-овские (uint64_t становится unsigned long). Отдельный «префикс» только с Vulkan.
rm -rf "$HEADERS"
mkdir -p "$HEADERS/include" "$HEADERS/share/cmake/VulkanHeaders"
ln -s /usr/include/vulkan "$HEADERS/include/vulkan"
ln -s /usr/include/vk_video "$HEADERS/include/vk_video"
ln -s /usr/share/vulkan "$HEADERS/share/vulkan"
cp /usr/share/cmake/VulkanHeaders/VulkanHeadersConfig*.cmake "$HEADERS/share/cmake/VulkanHeaders/"

echo "== сборка vulkan-1.dll (clang-cl, статический CRT)"
rm -rf "$BUILD"; mkdir -p "$WORK"
# USE_GAS: ассемблерная часть через clang-cl вместо MASM, которого здесь нет; поддержка неизвестных расширений Vulkan остаётся
cmake -S "$SRC" -B "$BUILD" -G Ninja \
  -DCMAKE_TOOLCHAIN_FILE="$HERE/vulkan/loader-toolchain.cmake" -DXWIN_TOOLCHAIN="$XWIN_TOOLCHAIN" -DXWIN_ROOT="$HOME/.cache/cargo-xwin/xwin" \
  -DCMAKE_BUILD_TYPE=Release -DUSE_GAS=ON \
  -DVulkanHeaders_DIR="$HEADERS/share/cmake/VulkanHeaders" \
  -DBUILD_TESTS=OFF -DBUILD_WERROR=OFF >"$BUILD.log" 2>&1 || { tail -30 "$BUILD.log"; exit 1; }
if grep -q "Could not find working" "$BUILD.log"; then
  echo "Ассемблер не заработал: загрузчик получится без поддержки неизвестных функций расширений."
  grep -A3 "Could not find working" "$BUILD.log"
  exit 1
fi
cmake --build "$BUILD" >>"$BUILD.log" 2>&1 || { tail -40 "$BUILD.log"; exit 1; }

DLL="$BUILD/loader/vulkan-1.dll"
[ -f "$DLL" ] || { echo "Не получилась $DLL"; exit 1; }

echo "== проверка"
# Статический CRT: никаких VCRUNTIME/MSVCP/ucrtbase, иначе на чистой Windows 11 загрузчик не поднимется
imports=$(llvm-objdump -p "$DLL" | awk '/DLL Name:/ {print tolower($3)}' | sort -u)
bad=$(echo "$imports" | grep -E '^(vcruntime|msvcp|ucrtbase|msvcr|concrt)' || true)
[ -z "$bad" ] || { echo "Загрузчик тянет библиотеки среды выполнения Visual C++: $bad"; exit 1; }
# Все функции из списка экспорта (по ним собрана import-библиотека, с которой линкуются Whisper и llama.cpp) на месте
exported=$(llvm-objdump -p "$DLL" | awk '/^ +[0-9]+ +0x[0-9a-f]+ +vk/ {print $3}' | sort -u)
wanted=$(sed -n '/^EXPORTS/,$p' "$HERE/vulkan/vulkan-1.def" | tr -d '\r' | awk 'NR>1 && NF {print $1}' | sort -u)
missing=$(comm -13 <(echo "$exported") <(echo "$wanted"))
[ -z "$missing" ] || { echo "В vulkan-1.dll нет функций из vulkan-1.def:"; echo "$missing"; exit 1; }
echo "  импорты: $(echo "$imports" | tr '\n' ' ')"
echo "  экспортов: $(echo "$wanted" | wc -l), все на месте"

mkdir -p "$OUT"
cp "$DLL" "$OUT/vulkan-1.dll"
cp "$SRC/LICENSE.txt" "$OUT/LICENSE-Vulkan-Loader.txt"
cat > "$OUT/README-vulkan.txt" <<TXT
vulkan-1.dll: загрузчик Vulkan (Khronos Vulkan-Loader $TAG, лицензия Apache-2.0, текст в LICENSE-Vulkan-Loader.txt).
Исходники: $URL, коммит $COMMIT.
Собран из них без изменений. Лежит рядом с программой, чтобы она запускалась и без драйвера Vulkan
(тогда анализ идёт на процессоре); при установленном драйвере видеокарты загрузчик находит его сам.
TXT
echo "$MARK" > "$STAMP"
ls -l "$OUT/vulkan-1.dll" | awk '{print "готово: " $9 " (" $5 " байт)"}'
