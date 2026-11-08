mod analysis_cmds;
mod assets_cmds;
mod commands;
mod db;
mod drafts;
pub mod analyze;
pub mod render;
pub mod tiktok;
pub mod youtube;
#[cfg(debug_assertions)]
mod dev_tools;
mod media;
mod media_server;
pub mod models;
pub mod platform;
mod state;
mod tiktok_cmds;
pub mod vulkan_loader;
mod youtube_cmds;

use tauri::Manager;

pub fn run() {
    // До первого обращения к Vulkan (Whisper подключает его при запуске), иначе на Windows без драйвера видеокарты не будет ни одного загрузчика
    let vulkan = vulkan_loader::prepare();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "avtonarezka_lib=info".into()),
        )
        .init();
    match vulkan {
        vulkan_loader::Source::System => {}
        vulkan_loader::Source::Bundled => tracing::info!("системного vulkan-1.dll нет (драйвер видеокарты не поставлен), взят запасной загрузчик из установки; без драйвера анализ идёт на процессоре"),
        vulkan_loader::Source::Missing => tracing::error!("нет ни системного, ни запасного vulkan-1.dll (папка vk рядом с программой): запуск анализа завершит программу"),
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // «Local», а не Roaming: на Windows здесь лежат модели на 3 ГБ, их незачем синхронизировать по домену.
            // На Linux путь тот же, что у `app_data_dir`.
            let data_dir = app.path().app_local_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let db = db::open(&data_dir.join("avtonarezka.db"))?;
            let media = tauri::async_runtime::block_on(media_server::MediaServer::start(app.handle().clone(), &data_dir))?;
            install_fonts(&data_dir.join("fonts"))?;
            drafts::recover_interrupted(&db.0.lock().unwrap());
            tiktok_cmds::recover_interrupted_posts(&db.0.lock().unwrap());
            app.manage(state::AppState {
                db,
                media,
                data_dir,
                analyses: Default::default(),
                renders: Default::default(),
                draft_slots: std::sync::Arc::new(tokio::sync::Semaphore::new(2)),
                final_slots: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
                nvenc: Default::default(),
                secrets: tiktok::secrets::Secrets::detect(),
                login: Default::default(),
                posting: Default::default(),
                downloads: Default::default(),
                youtube_slots: std::sync::Arc::new(tokio::sync::Semaphore::new(2)),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::media_url,
            commands::import_video,
            commands::list_projects,
            commands::delete_project,
            commands::project_set_tight,
            analysis_cmds::models_status,
            analysis_cmds::analysis_start,
            analysis_cmds::analysis_cancel,
            analysis_cmds::analysis_notes,
            analysis_cmds::moments_list,
            analysis_cmds::moment_set_status,
            analysis_cmds::moment_update,
            analysis_cmds::moment_add_manual,
            assets_cmds::assets_list,
            assets_cmds::asset_add,
            assets_cmds::asset_delete,
            drafts::drafts_generate,
            drafts::drafts_list,
            drafts::draft_set_status,
            drafts::drafts_approve_ready,
            drafts::draft_update,
            drafts::draft_rerender,
            drafts::draft_cancel,
            drafts::defaults_get,
            drafts::defaults_set,
            drafts::finals_start,
            drafts::output_dir_get,
            drafts::output_dir_set,
            drafts::output_dir_open,
            drafts::final_open,
            tiktok_cmds::tiktok_status,
            tiktok_cmds::tiktok_save_settings,
            tiktok_cmds::tiktok_forget_secret,
            tiktok_cmds::tiktok_login_start,
            tiktok_cmds::tiktok_login_cancel,
            tiktok_cmds::tiktok_logout,
            tiktok_cmds::tiktok_publish,
            tiktok_cmds::posts_list,
            youtube_cmds::youtube_analyze,
            youtube_cmds::youtube_download,
            youtube_cmds::youtube_cancel,
        ])
        .run(tauri::generate_context!())
        .expect("ошибка запуска Автонарезки");
}

/// Шрифты субтитров вшиты в программу и кладутся рядом с данными: libass читает их из папки.
fn install_fonts(dir: &std::path::Path) -> std::io::Result<()> {
    const FONTS: [(&str, &[u8]); 3] = [
        ("Montserrat_700Bold.ttf", include_bytes!("../../assets/fonts/Montserrat_700Bold.ttf")),
        ("Montserrat_800ExtraBold.ttf", include_bytes!("../../assets/fonts/Montserrat_800ExtraBold.ttf")),
        ("Montserrat_900Black.ttf", include_bytes!("../../assets/fonts/Montserrat_900Black.ttf")),
    ];
    std::fs::create_dir_all(dir)?;
    for (name, bytes) in FONTS {
        let path = dir.join(name);
        if std::fs::metadata(&path).map_or(true, |m| m.len() != bytes.len() as u64) {
            std::fs::write(path, bytes)?;
        }
    }
    Ok(())
}
