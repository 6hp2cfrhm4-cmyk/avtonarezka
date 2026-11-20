//! Безголовый демон фабрики: без окна, видеокарты и D-Bus. Запускается на сервере под systemd.
fn main() -> anyhow::Result<()> {
    avtonarezka_lib::run_headless()
}
