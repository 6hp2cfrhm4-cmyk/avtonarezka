# Тулчейн для сборки загрузчика Vulkan (vulkan-1.dll) кросс-компиляцией: тулчейн cargo-xwin плюс правило архивации
# для ассемблерных файлов. У CMake оно по умолчанию в стиле GNU (`ar cr`), а llvm-lib такого не понимает, и из-за этого
# проверка «работает ли ассемблер» (try_compile в loader/CMakeLists.txt) падает, а загрузчик теряет поддержку
# неизвестных функций расширений. Путь к тулчейну cargo-xwin передаёт windows/build-vulkan-loader.sh.
# Без этой строки try_compile-проекты CMake не видят XWIN_TOOLCHAIN и собирают пробные файлы под Linux
list(APPEND CMAKE_TRY_COMPILE_PLATFORM_VARIABLES XWIN_TOOLCHAIN)
include("${XWIN_TOOLCHAIN}")

set(CMAKE_ASM_CREATE_STATIC_LIBRARY "<CMAKE_AR> /nologo /out:<TARGET> <LINK_FLAGS> <OBJECTS>")

# Ресурсный файл загрузчика (loader.rc) подключает winres.h из Windows SDK, а к компилятору ресурсов тулчейн cargo-xwin
# пути не передаёт (переменная INCLUDE на Linux-хосте не читается)
set(CMAKE_RC_STANDARD_INCLUDE_DIRECTORIES
    "${XWIN_ROOT}/sdk/include/um" "${XWIN_ROOT}/sdk/include/shared"
    "${XWIN_ROOT}/sdk/include/ucrt" "${XWIN_ROOT}/crt/include")

# Тулчейн cargo-xwin обнуляет стандартные библиотеки, а загрузчик берёт из реестра и из токена процесса данные
# драйверов (RegOpenKeyEx, OpenProcessToken…): все эти функции лежат в advapi32
set(CMAKE_C_STANDARD_LIBRARIES "advapi32.lib" CACHE STRING "" FORCE)
