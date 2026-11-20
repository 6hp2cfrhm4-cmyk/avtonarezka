//! Фоновый присмотр за переданными задачами.
//! Демон на сервере подхватывает пришедшие упаковки (запускает раннер фабрики).
//! Окно на ПК само забирает готовые ролики, пока есть переданные задачи и связь разрешена предохранителем.
use std::time::Duration;

use crate::{
    host::{AppHandle, Manager},
    state::AppState,
};

pub fn start(app: &AppHandle) {
    let app = app.clone();
    crate::host::spawn(async move {
        loop {
            tick(&app).await;
            tokio::time::sleep(Duration::from_secs(if cfg!(feature = "desktop") { 90 } else { 15 })).await;
        }
    });
}

async fn tick(app: &AppHandle) {
    #[cfg(not(feature = "desktop"))]
    {
        // Принятые задачи ждут раннер: он сам выходит, когда делать нечего.
        crate::factory::start(app);
    }
    #[cfg(feature = "desktop")]
    {
        let state = app.state::<AppState>();
        let outstanding: bool = {
            let db = state.db.0.lock().unwrap();
            let enabled = super::link::load_config(&db).is_some_and(|c| c.enabled);
            enabled
                && db
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM transfers WHERE direction='out' AND state='accepted')",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(false)
        };
        // «Я включён»: короткий опрос раз в круг, чтобы сервер знал, что ПК на связи (и переключил сцену обратно)
        let enabled = super::link::load_config(&state.db.0.lock().unwrap()).is_some_and(|c| c.enabled);
        if enabled && !outstanding {
            let _ = super::cmds::node_check(app.clone()).await;
        }
        if outstanding {
            // ошибки связи учитывает предохранитель, повторять немедленно не нужно
            let _ = super::cmds::node_pull(app.clone()).await;
        }
    }
    let _ = app.state::<AppState>();
}
