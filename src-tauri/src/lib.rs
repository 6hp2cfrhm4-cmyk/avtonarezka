// Команды окна без Tauri никто не вызывает: в безголовой сборке они остаются обычными функциями.
#![cfg_attr(not(feature = "desktop"), allow(dead_code, unused_imports))]
mod accounts;
mod analysis_cmds;
mod analysis_inputs;
pub mod analytics;
pub mod analyze;
mod assets_cmds;
mod camera;
mod campaigns;
mod campaigns_report;
pub mod caption;
mod commands;
mod db;
#[cfg(all(debug_assertions, feature = "desktop"))]
mod dev_tools;
mod drafts;
mod factory;
mod gameplay;
pub mod host;
mod media;
mod media_server;
mod model_download;
pub mod models;
pub mod discovery;
pub mod live;
pub mod notifier;
pub mod node;
pub mod workflows;
mod notifications;
pub mod operations;
pub mod platform;
pub mod publish;
mod publish_queue;
pub mod render;
pub mod sources;
mod state;
pub mod tiktok;
pub mod youtube;
use factory as batch;
mod ready;
mod tiktok_cmds;
pub mod vulkan_loader;
mod youtube_cmds;

#[cfg(feature = "desktop")]
use tauri::Manager;

/// Общая инициализация ядра: база, состояние и фоновые циклы. Одинакова в окне и в демоне.
pub async fn init_state(app: &host::AppHandle, data_dir: std::path::PathBuf) -> anyhow::Result<()> {
    #[allow(unused_imports)]
    use host::Manager;
    std::fs::create_dir_all(&data_dir)?;
    if let Err(e) = operations::diagnostics::recorder().bind(&data_dir) {
        tracing::warn!("Не удалось открыть структурированный журнал: {e}");
    }
    let db = db::open(&data_dir.join("avtonarezka.db"))?;
    let media = media_server::MediaServer::start(app.clone(), &data_dir).await?;
    install_fonts(&data_dir.join("fonts"))?;
    drafts::recover_interrupted(&db.0.lock().unwrap());
    tiktok_cmds::recover_interrupted_posts(&db.0.lock().unwrap());
    publish_queue::recover_interrupted_jobs(&db.0.lock().unwrap());
    publish::calendar::recover(&db.0.lock().unwrap());
    batch::recover_interrupted(&db.0.lock().unwrap());
    let factory_settings = factory::resources::load(&db.0.lock().unwrap());
    #[cfg(feature = "desktop")]
    let secrets = tiktok::secrets::Secrets::detect();
    #[cfg(not(feature = "desktop"))]
    let secrets = tiktok::secrets::Secrets::detect_headless(&data_dir);
    app.manage(state::AppState {
        source_use: Default::default(),
        power: Default::default(),
        lifecycle: Default::default(),
        db,
        media,
        data_dir,
        analyses: Default::default(),
        camera_scan: Default::default(),
        renders: Default::default(),
        draft_slots: factory::resources::Lane::new(factory_settings.draft_concurrency),
        final_slots: factory::resources::Lane::new(factory_settings.final_concurrency),
        ml_gate: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
        nvenc: Default::default(),
        cuda: Default::default(),
        secrets,
        login: Default::default(),
        platform_login: Default::default(),
        posting: Default::default(),
        browser_gate: Default::default(),
        models_downloading: Default::default(),
        publisher: Default::default(),
        drafting: Default::default(),
        queue_note: Default::default(),
        downloads: Default::default(),
        youtube_slots: std::sync::Arc::new(tokio::sync::Semaphore::new(2)),
        batch: Default::default(),
        monitor: factory::monitor::MonitorState::new(factory_settings.resume_on_start),
        source_prepares: Default::default(),
        output_prepares: Default::default(),
        analytics_gate: Default::default(),
    });
    // Загрузка моделей запускается только явной кнопкой пользователя.
    batch::resume(app);
    factory::monitor::start(app);
    analytics::start(app);
    operations::power::start(app);
    operations::disk::start(app);
    operations::lifecycle::restore(app);
    publish_queue::resume(app);
    node::runner::start(app);
    workflows::runtime::start(app);
    discovery::runtime::start(app);
    live::runtime::start(app);
    notifier::start(app);
    Ok(())
}

/// Каталог данных демона: `AVTO_DATA_DIR`, иначе `$XDG_DATA_HOME/avtonarezka-daemon`.
pub fn daemon_data_dir() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("AVTO_DATA_DIR") {
        return dir.into();
    }
    // Серверная установка: sudo не передаёт окружение, поэтому знакомый каталог берём сами.
    let server_dir = std::path::Path::new("/var/lib/avtonarezka");
    if server_dir.join("avtonarezka.db").is_file() {
        return server_dir.into();
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share"))
        })
        .unwrap_or_else(|| ".".into());
    base.join("avtonarezka-daemon")
}

#[cfg(not(feature = "desktop"))]
/// Демон без окна: то же ядро, события идут в шину `host`, остановка по SIGINT/SIGTERM.
pub fn run_headless() -> anyhow::Result<()> {
    use tracing_subscriber::prelude::*;
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "avtonarezka_lib=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .with(operations::diagnostics::layer())
        .init();
    let data_dir = daemon_data_dir();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let app = host::AppHandle::new();
        init_state(&app, data_dir.clone()).await?;
        tracing::info!("демон запущен, данные: {}", data_dir.display());
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
        tracing::info!("демон остановлен");
        anyhow::Ok(())
    })
}

#[cfg(feature = "desktop")]
pub fn run() {
    // До первого обращения к Vulkan (Whisper подключает его при запуске), иначе на Windows без драйвера видеокарты не будет ни одного загрузчика
    let vulkan = vulkan_loader::prepare();

    use tracing_subscriber::prelude::*;
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "avtonarezka_lib=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .with(operations::diagnostics::layer())
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
            tauri::async_runtime::block_on(init_state(app.handle(), data_dir))?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            notifier::notify_state,
            notifier::notify_set,
            notifier::notify_test,
            live::runtime::live_state,
            live::runtime::live_start,
            live::runtime::live_stop,
            live::runtime::live_cut_now,
            live::runtime::live_moment_ignore,
            discovery::runtime::integration_test,
            node::cmds::node_keygen,
            node::cmds::node_config_push,
            node::cmds::node_config_remote,
            discovery::runtime::discovery_state,
            discovery::runtime::discovery_config_set,
            discovery::runtime::discovery_secret_set,
            discovery::runtime::discovery_poll_now,
            discovery::runtime::discovery_accept,
            discovery::runtime::discovery_reject,
            workflows::runtime::modes_state,
            workflows::runtime::scene_activate,
            workflows::runtime::scene_unpin,
            workflows::runtime::scene_save,
            workflows::runtime::scene_delete,
            workflows::runtime::rule_save,
            workflows::runtime::rule_delete,
            node::cmds::node_config_get,
            node::cmds::node_config_set,
            node::cmds::node_fuse_reset,
            node::cmds::node_overview,
            node::cmds::node_check,
            node::cmds::node_hostkey_scan,
            node::cmds::node_hostkey_accept,
            node::cmds::node_candidates,
            node::cmds::node_send,
            node::cmds::node_pull,
            node::cmds::node_recall,
            node::cmds::node_handover,
            node::cmds::node_poweroff,
            operations::power::power_status,
            operations::lifecycle::lifecycle_status,
            operations::lifecycle::lifecycle_tray_set,
            operations::lifecycle::lifecycle_autostart_set,
            operations::lifecycle::lifecycle_hide,
            operations::disk::disk_snapshot,
            operations::diagnostics::diagnostics_collect,
            operations::diagnostics::diagnostics_export,
            operations::disk::disk_policy_save,
            operations::disk::disk_cleanup,
            notifications::notifications_enabled,
            notifications::notifications_set,
            analytics::analytics_dashboard,
            analytics::analytics_history,
            analytics::analytics_manual,
            analytics::analytics_auto,
            analytics::analytics_collect,
            analytics::vk::vk_metrics_options,
            analytics::vk::vk_metrics_account_save,
            analytics::vk::vk_metrics_account_enabled,
            analytics::vk::vk_metrics_link,
            analytics::feedback::analytics_feedback,
            analytics::feedback::analytics_apply,
            commands::media_url,
            commands::import_video,
            commands::list_projects,
            commands::delete_project,
            commands::project_set_tight,
            analysis_cmds::models_status,
            model_download::models_download,
            model_download::face_model_download,
            analysis_cmds::analysis_options_get,
            analysis_cmds::analysis_options_set,
            analysis_cmds::analysis_start,
            analysis_cmds::analysis_cancel,
            analysis_cmds::analysis_notes,
            analysis_inputs::analysis_inputs_get,
            camera::camera_frame,
            camera::camera_detect,
            camera::camera_scan,
            camera::camera_scan_cancel,
            camera::layout_preview,
            camera::face_model_ready,
            publish::platform_caps,
            publish::output::output_profiles,
            publish::output::output_profile_save,
            publish::output::output_preview,
            publish::calendar::publication_accounts,
            publish::calendar::publication_account_policy,
            publish::calendar::telegram_account_save,
            publish::calendar::telegram_account_check,
            publish::instagram::accounts::instagram_account_save,
            publish::youtube::accounts::youtube_oauth_settings,
            publish::youtube::accounts::youtube_oauth_save,
            publish::youtube::accounts::youtube_login_start,
            publish::youtube::accounts::youtube_login_cancel,
            publish::youtube::accounts::youtube_account_save,
            publish::calendar::publications_list,
            publish::calendar::publication_schedule,
            publish::autofill::publication_autofill,
            publish::calendar::publication_move,
            publish::calendar::publication_refresh_caption,
            publish::calendar::publication_cancel,
            publish::calendar::publication_resolve,
            publish_queue::publications_start,
            caption::caption_preview,
            campaigns::campaigns_list,
            campaigns::campaign_save,
            campaigns::campaign_preview,
            campaigns::campaign_assignments,
            campaigns_report::campaign_report_rows,
            campaigns_report::campaign_report_export,
            campaigns_report::campaign_report_open,
            caption::commands::caption_draft_load,
            caption::commands::caption_draft_save,
            caption::commands::caption_apply_body,
            caption::commands::caption_draft_generate,
            caption::commands::caption_account_load,
            caption::commands::caption_account_save,
            analysis_inputs::analysis_chat_import,
            analysis_inputs::analysis_chat_clear,
            analysis_inputs::analysis_timeline,
            analysis_inputs::analysis_twitch_clips,
            analysis_inputs::analysis_clips_clear,
            analysis_inputs::twitch_signal_status,
            analysis_inputs::twitch_signal_credentials,
            analysis_cmds::moments_list,
            analysis_cmds::moment_set_status,
            analysis_cmds::moment_update,
            analysis_cmds::moment_add_manual,
            assets_cmds::assets_list,
            assets_cmds::asset_add,
            assets_cmds::asset_delete,
            gameplay::gameplay_tape,
            gameplay::gameplay_tape_reset,
            drafts::drafts_generate,
            drafts::drafts_list,
            drafts::draft_set_status,
            drafts::drafts_approve_ready,
            drafts::draft_update,
            drafts::draft_rerender,
            drafts::drafts_realign_gameplay,
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
            publish_queue::queue_state,
            publish_queue::queue_prepare,
            publish_queue::queue_prepare_cancel,
            publish_queue::queue_apply_text,
            publish_queue::queue_job_update,
            publish_queue::queue_job_resolve,
            publish_queue::queue_settings_set,
            publish_queue::queue_start,
            publish_queue::queue_pause,
            publish_queue::queue_stop,
            factory::factory_calendar,
            factory::factory_state,
            factory::factory_item_update,
            factory::factory_decide,
            factory::resources::factory_settings,
            factory::resources::factory_settings_set,
            factory::profiles::channels_list,
            factory::profiles::channel_save,
            factory::profiles::channel_remove,
            factory::rights::channel_incidents,
            factory::rights::channel_incident_add,
            factory::rights::channel_incident_resolve,
            factory::rights::channel_rights_resume,
            factory::monitor::source_check,
            factory::profiles::templates_list,
            factory::profiles::template_save,
            factory::profiles::template_remove,
            batch::batch_list,
            batch::batch_add,
            batch::batch_start,
            batch::batch_cancel,
            batch::batch_remove,
            batch::batch_retry,
            batch::batch_approve_ready,
            ready::ready_add,
            ready::ready_remove,
            publish_queue::queue_job_account,
            accounts::accounts_list,
            accounts::accounts_add,
            accounts::accounts_set_enabled,
            accounts::accounts_remove,
            accounts::accounts_open_login,
            youtube_cmds::youtube_analyze,
            youtube_cmds::youtube_download,
            youtube_cmds::youtube_cancel,
        ])
        .run(tauri::generate_context!())
        .expect("ошибка запуска Автонарезки");
}

/// Шрифты субтитров вшиты в программу и кладутся рядом с данными: libass читает их из папки.
pub(crate) fn install_fonts(dir: &std::path::Path) -> std::io::Result<()> {
    const FONTS: [(&str, &[u8]); 3] = [
        (
            "Montserrat_700Bold.ttf",
            include_bytes!("../../assets/fonts/Montserrat_700Bold.ttf"),
        ),
        (
            "Montserrat_800ExtraBold.ttf",
            include_bytes!("../../assets/fonts/Montserrat_800ExtraBold.ttf"),
        ),
        (
            "Montserrat_900Black.ttf",
            include_bytes!("../../assets/fonts/Montserrat_900Black.ttf"),
        ),
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
