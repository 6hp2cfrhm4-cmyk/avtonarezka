fn main() {
    tauri_build::build();

    // Windows со статическим CRT (`windows/env.sh`): математика (sin, pow, exp…) лежит в libucrt.lib, но в
    // команде линковки её никто не называет, а чисто Rust-зависимости (tract) без неё не собираются.
    let crt_static = std::env::var("CARGO_CFG_TARGET_FEATURE").is_ok_and(|f| f.split(',').any(|x| x == "crt-static"));
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") && crt_static {
        println!("cargo:rustc-link-lib=libucrt");
    }
    // vulkan-1.dll подключаем с отложенной загрузкой, чтобы программа запускалась и без драйвера видеокарты
    // (src/vulkan_loader.rs выбирает системный загрузчик или запасной из установки до первого обращения к Vulkan)
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg=/DELAYLOAD:vulkan-1.dll");
        println!("cargo:rustc-link-lib=delayimp");
    }
}
