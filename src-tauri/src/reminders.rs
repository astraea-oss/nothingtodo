//! Reminder delivery. On Android, upcoming reminders are handed to the OS so they fire with the
//! app closed. Desktop checks in-process, which works because closing the window keeps the app
//! running in the tray.
use crate::AppState;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc;

/// Matches the Android notification title in `Reminders.kt`.
#[cfg(not(target_os = "android"))]
const TITLE: &str = "NothingToDo reminder";

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Asks for delivery to catch up with the store after an edit, sync or snooze. Requests that
/// arrive while one is being handled are merged.
pub fn request(app: &AppHandle) {
    let _ = app.state::<AppState>().reminder_trigger.try_send(());
}

fn report(app: &AppHandle, error: String) {
    eprintln!("NothingToDo reminders: {error}");
    let _ = app.emit("reminder-error", error);
}

#[cfg(target_os = "android")]
pub use android::{plugin, run};

#[cfg(target_os = "android")]
mod android {
    use super::{AppState, Manager, mpsc, now_ms, report};
    use serde_json::json;
    use tauri::{
        AppHandle, Wry,
        plugin::{PluginHandle, TauriPlugin},
    };

    /// `ReminderPlugin` in the Android project (`Reminders.kt`): saves the list, cancels alarms
    /// that dropped off it and arms the rest. Its receivers post the notification and re-arm after
    /// a reboot without the app running.
    struct Scheduler(PluginHandle<Wry>);

    pub fn plugin() -> TauriPlugin<Wry> {
        tauri::plugin::Builder::new("nothingtodo-reminders")
            .setup(|app, api| {
                let handle =
                    api.register_android_plugin("uk.co.astraeaoss.nothingtodo", "ReminderPlugin")?;
                app.manage(Scheduler(handle));
                Ok(())
            })
            .build()
    }

    pub async fn run(app: AppHandle, mut requests: mpsc::Receiver<()>) {
        while requests.recv().await.is_some() {
            while requests.try_recv().is_ok() {}
            // At launch the plugin's Android side may still be registering, so retry briefly.
            let mut attempt = 1;
            while let Err(error) = reconcile(&app).await {
                if attempt == 4 {
                    report(&app, error);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_secs(attempt)).await;
                attempt += 1;
            }
        }
    }

    async fn reconcile(app: &AppHandle) -> Result<(), String> {
        let alarms = app
            .state::<AppState>()
            .store
            .lock()
            .await
            .alarms(now_ms())?;
        let reminders: Vec<_> = alarms
            .iter()
            .map(|a| json!({ "id": a.notification_id, "at": a.at, "title": a.title }))
            .collect();
        let app = app.clone();
        // Plugin calls block on the Android side.
        tauri::async_runtime::spawn_blocking(move || {
            app.state::<Scheduler>()
                .0
                .run_mobile_plugin::<()>("schedule", json!({ "reminders": reminders }))
                .map_err(|e| format!("Could not schedule reminders: {e}"))
        })
        .await
        .map_err(|_| "Reminder scheduling stopped unexpectedly.".to_string())?
    }
}

/// Checks every few seconds while the app runs and shows due reminders directly.
#[cfg(not(target_os = "android"))]
pub async fn run(app: AppHandle, _requests: mpsc::Receiver<()>) {
    use tauri_plugin_notification::{NotificationExt, PermissionState};

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        if app.notification().permission_state().ok() != Some(PermissionState::Granted) {
            continue;
        }
        let state = app.state::<AppState>();
        let mut store = state.store.lock().await;
        for (id, task, due) in store.due(now_ms()) {
            let shown = app
                .notification()
                .builder()
                .title(TITLE)
                .body(&task.title)
                .show();
            if shown.is_err() {
                continue;
            }
            if let Err(error) = store.mark_notified(&id, due) {
                report(&app, format!("Could not save reminder receipt: {error}"));
            }
        }
    }
}
