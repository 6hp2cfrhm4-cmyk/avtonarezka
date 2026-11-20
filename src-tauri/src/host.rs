//! Слой между ядром фабрики и оболочкой.
//!
//! В десктопной сборке (`feature = "desktop"`) это просто Tauri. В безголовой сборке (демон на сервере)
//! те же имена `AppHandle`, `State`, `Manager`, `Emitter` реализованы здесь без окна, без GTK и без D-Bus:
//! состояние лежит в самом `AppHandle`, события идут в канал `broadcast`.

#[cfg(feature = "desktop")]
pub use tauri::{AppHandle, Emitter, Manager, State};

/// Запуск фоновой задачи в рантайме приложения.
#[cfg(feature = "desktop")]
pub fn spawn<F>(fut: F) -> tauri::async_runtime::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    tauri::async_runtime::spawn(fut)
}

#[cfg(not(feature = "desktop"))]
pub use headless::*;

#[cfg(not(feature = "desktop"))]
mod headless {
    use std::{
        any::Any,
        ops::Deref,
        sync::{Arc, OnceLock},
    };

    use serde::Serialize;
    use tokio::sync::broadcast;

    /// Событие для подписчиков (в десктопе его получает интерфейс, в демоне — удалённый клиент и журнал).
    #[derive(Clone, Debug)]
    pub struct Event {
        pub name: String,
        pub payload: serde_json::Value,
    }

    struct Inner {
        state: OnceLock<Box<dyn Any + Send + Sync>>,
        bus: broadcast::Sender<Event>,
    }

    /// Ручка приложения без окна: одно управляемое состояние и шина событий.
    #[derive(Clone)]
    pub struct AppHandle {
        inner: Arc<Inner>,
    }

    impl Default for AppHandle {
        fn default() -> Self {
            Self::new()
        }
    }

    impl AppHandle {
        pub fn new() -> Self {
            let (bus, _) = broadcast::channel(1024);
            Self {
                inner: Arc::new(Inner {
                    state: OnceLock::new(),
                    bus,
                }),
            }
        }

        /// Кладёт состояние приложения; второй вызов игнорируется.
        pub fn manage<T: Send + Sync + 'static>(&self, state: T) -> bool {
            self.inner.state.set(Box::new(state)).is_ok()
        }

        pub fn subscribe(&self) -> broadcast::Receiver<Event> {
            self.inner.bus.subscribe()
        }

        pub fn handle(&self) -> &AppHandle {
            self
        }
    }

    /// Доступ к состоянию, как `tauri::State`.
    pub struct State<'a, T: Send + Sync + 'static>(&'a T);

    impl<T: Send + Sync + 'static> Clone for State<'_, T> {
        fn clone(&self) -> Self {
            State(self.0)
        }
    }

    impl<'a, T: Send + Sync + 'static> State<'a, T> {
        pub fn inner(&self) -> &'a T {
            self.0
        }
    }

    impl<T: Send + Sync + 'static> Deref for State<'_, T> {
        type Target = T;
        fn deref(&self) -> &T {
            self.0
        }
    }

    pub trait Manager {
        fn state<T: Send + Sync + 'static>(&self) -> State<'_, T>;
    }

    impl Manager for AppHandle {
        fn state<T: Send + Sync + 'static>(&self) -> State<'_, T> {
            let any = self
                .inner
                .state
                .get()
                .expect("состояние приложения не задано (AppHandle::manage)");
            State(
                any.downcast_ref::<T>()
                    .expect("запрошено состояние другого типа"),
            )
        }
    }

    pub trait Emitter {
        fn emit<S: Serialize + Clone>(
            &self,
            event: &str,
            payload: S,
        ) -> Result<(), serde_json::Error>;
    }

    impl Emitter for AppHandle {
        fn emit<S: Serialize + Clone>(
            &self,
            event: &str,
            payload: S,
        ) -> Result<(), serde_json::Error> {
            let payload = serde_json::to_value(payload)?;
            // нет подписчиков — не ошибка
            let _ = self.inner.bus.send(Event {
                name: event.to_string(),
                payload,
            });
            Ok(())
        }
    }

    pub fn spawn<F>(fut: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        tokio::spawn(fut)
    }
}
