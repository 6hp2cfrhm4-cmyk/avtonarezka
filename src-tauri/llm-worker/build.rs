fn main() {
    // vulkan-1.dll подключаем с отложенной загрузкой, чтобы воркер запускался и без драйвера видеокарты
    // (../src/vulkan_loader.rs выбирает системный загрузчик или запасной из установки до первого обращения к Vulkan)
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg=/DELAYLOAD:vulkan-1.dll");
        println!("cargo:rustc-link-lib=delayimp");
    }
}
