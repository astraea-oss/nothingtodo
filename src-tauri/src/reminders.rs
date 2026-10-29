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
pub use android::{open_notification_settings, plugin, run, save_to_downloads};

#[cfg(not(target_os = "android"))]
pub fn open_notification_settings(_: &AppHandle) -> Result<(), String> {
    Err("Turn on notifications for NothingToDo in your desktop's notification settings.".into())
}

#[cfg(target_os = "android")]
mod android {
    use super::{AppState, Emitter, Manager, mpsc, now_ms, report};
    use nothingtodo_core::alarms::NotificationAction;
    use serde_json::json;
    use tauri::{
        AppHandle, Wry,
        plugin::{PluginHandle, TauriPlugin},
    };

    /// `ReminderPlugin` in the Android project (`Reminders.kt`): saves the list, cancels alarms
    /// that dropped off it and arms the rest. Its receivers post the notification and re-arm after
    /// a reboot without the app running. It also hosts the app's other small Android helpers.
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

    pub fn save_to_downloads(app: &AppHandle, name: &str, text: &str) -> Result<String, String> {
        #[derive(serde::Deserialize)]
        struct Saved {
            location: String,
        }
        app.state::<Scheduler>()
            .0
            .run_mobile_plugin::<Saved>("saveToDownloads", json!({ "name": name, "text": text }))
            .map(|saved| saved.location)
            .map_err(|e| format!("Could not save the backup: {e}"))
    }

    pub fn open_notification_settings(app: &AppHandle) -> Result<(), String> {
        app.state::<Scheduler>()
            .0
            .run_mobile_plugin::<()>("openNotificationSettings", ())
            .map_err(|e| format!("Could not open notification settings: {e}"))
    }

    /// A notification button press from `Reminders.kt`'s action log.
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Pressed {
        key: String,
        task_id: String,
        remind_at: i64,
        #[serde(flatten)]
        action: NotificationAction,
    }

    #[derive(serde::Deserialize)]
    struct Actions {
        actions: Vec<Pressed>,
    }

    /// Runs a blocking plugin call off the async threads.
    async fn call<T: serde::de::DeserializeOwned + Send + 'static>(
        app: &AppHandle,
        command: &'static str,
        payload: serde_json::Value,
    ) -> Result<T, String> {
        let app = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            app.state::<Scheduler>()
                .0
                .run_mobile_plugin(command, payload)
                .map_err(|e| format!("Reminder scheduling failed ({command}): {e}"))
        })
        .await
        .map_err(|_| "Reminder scheduling stopped unexpectedly.".to_string())?
    }

    /// Applies notification button presses, then hands the current list to Android. Presses are
    /// applied before the list is built, so a reminder completed from its notification is not
    /// scheduled again.
    async fn reconcile(app: &AppHandle) -> Result<(), String> {
        let pressed: Actions = call(app, "actions", json!({})).await?;
        let state = app.state::<AppState>();
        if !pressed.actions.is_empty() {
            let mut changed = false;
            {
                let mut store = state.store.lock().await;
                for p in &pressed.actions {
                    changed |= store.apply_notification_action(
                        &p.task_id,
                        p.remind_at,
                        &p.action,
                        now_ms(),
                    )?;
                }
            }
            if changed {
                let _ = state
                    .sync_trigger
                    .try_send(crate::sync_worker::Trigger::Edited);
            }
            let _ = app.emit("tasks-changed", ());
            let keys: Vec<&str> = pressed.actions.iter().map(|p| p.key.as_str()).collect();
            call::<()>(app, "acknowledgeActions", json!({ "keys": keys })).await?;
        }
        let alarms = state.store.lock().await.alarms(now_ms())?;
        let reminders: Vec<_> = alarms
            .iter()
            .map(|a| {
                json!({
                    "id": a.notification_id,
                    "at": a.at,
                    "title": a.title,
                    "taskId": a.task_id,
                    "remindAt": a.remind_at,
                    "warning": a.warning,
                    "ring": a.ring,
                })
            })
            .collect();
        call(app, "schedule", json!({ "reminders": reminders })).await
    }
}

/// Checks every few seconds while the app runs and shows due reminders directly.
#[cfg(not(target_os = "android"))]
pub async fn run(app: AppHandle, _requests: mpsc::Receiver<()>) {
    use tauri_plugin_notification::{NotificationExt, PermissionState};

    // Warnings fire as their time passes while the app runs; ones missed while it was closed are
    // not sent late, matching the missed-reminder rule.
    let mut checked = now_ms();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let now = now_ms();
        let state = app.state::<AppState>();
        let mut store = state.store.lock().await;
        for (id, task, due) in store.due(now) {
            if task.kind() != nothingtodo_core::entries::Kind::Alarm {
                continue;
            }
            // Alarms ring from the app itself, so they need no notification permission.
            ring::start(&app, &id, &task.title, due);
            if let Err(error) = store.mark_notified(&id, due) {
                report(&app, format!("Could not save alarm receipt: {error}"));
            }
        }
        if app.notification().permission_state().ok() != Some(PermissionState::Granted) {
            checked = now;
            continue;
        }
        for (_, title) in store.warnings_between(checked, now) {
            let _ = app
                .notification()
                .builder()
                .title(TITLE)
                .body(&title)
                .show();
        }
        checked = now;
        for (id, task, due) in store.due(now) {
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

/// Desktop alarms: while the app runs, a due alarm brings the window forward and loops the
/// system alarm sound until it is stopped or snoozed, for at most 10 minutes. Nothing rings
/// while the app is closed.
#[cfg(not(target_os = "android"))]
pub mod ring {
    use super::{AppState, now_ms};
    use serde::Serialize;
    use std::process::Command;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use tauri::{AppHandle, Emitter, Manager};

    const LIMIT_MS: i64 = 10 * 60 * 1000;
    const SOUNDS: [&str; 2] = [
        "/usr/share/sounds/freedesktop/stereo/alarm-clock-elapsed.oga",
        "/usr/share/sounds/freedesktop/stereo/complete.oga",
    ];

    #[derive(Clone, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Ringing {
        pub task_id: String,
        pub title: String,
        pub at: i64,
        #[serde(skip)]
        stop: Arc<AtomicBool>,
    }

    static RINGING: Mutex<Option<Ringing>> = Mutex::new(None);

    pub fn current() -> Option<Ringing> {
        RINGING.lock().ok()?.clone()
    }

    /// Plays one round of the alarm sound with whatever the desktop offers.
    fn play_once() -> bool {
        let file = SOUNDS.iter().find(|f| std::path::Path::new(f).exists());
        let tries: Vec<(&str, Vec<&str>)> = match file {
            Some(f) => vec![
                ("pw-play", vec![*f]),
                ("paplay", vec![*f]),
                ("canberra-gtk-play", vec!["-i", "alarm-clock-elapsed"]),
            ],
            None => vec![("canberra-gtk-play", vec!["-i", "alarm-clock-elapsed"])],
        };
        tries.into_iter().any(|(cmd, args)| {
            Command::new(cmd)
                .args(args)
                .status()
                .is_ok_and(|s| s.success())
        })
    }

    pub fn start(app: &AppHandle, task_id: &str, title: &str, at: i64) {
        stop();
        let ringing = Ringing {
            task_id: task_id.into(),
            title: title.into(),
            at,
            stop: Arc::new(AtomicBool::new(false)),
        };
        if let Ok(mut slot) = RINGING.lock() {
            *slot = Some(ringing.clone());
        }
        crate::window::show_main(app);
        {
            use tauri_plugin_notification::NotificationExt;
            let _ = app
                .notification()
                .builder()
                .title("NothingToDo alarm")
                .body(title)
                .show();
        }
        let _ = app.emit("alarm-ringing", ringing.clone());
        let flag = ringing.stop.clone();
        let app = app.clone();
        std::thread::spawn(move || {
            let started = now_ms();
            while !flag.load(Ordering::SeqCst) && now_ms() - started < LIMIT_MS {
                if !play_once() {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
            }
            // Unanswered: stop ringing; the alarm stays as an overdue entry.
            if !flag.load(Ordering::SeqCst) {
                stop();
                let _ = app.emit("alarm-ringing", None::<Ringing>);
            }
        });
    }

    pub fn stop() -> Option<Ringing> {
        let taken = RINGING.lock().ok()?.take();
        if let Some(r) = &taken {
            r.stop.store(true, Ordering::SeqCst);
        }
        taken
    }

    /// Stop counts as done: a one-off alarm completes and a repeating one moves on. Snooze moves it
    /// 10 minutes later on this device only.
    pub async fn answer(app: &AppHandle, snooze: bool) -> Result<(), String> {
        let Some(ringing) = stop() else { return Ok(()) };
        let _ = app.emit("alarm-ringing", None::<Ringing>);
        let state = app.state::<AppState>();
        {
            let mut store = state.store.lock().await;
            let now = now_ms();
            if snooze {
                store.snooze(&ringing.task_id, now, now + 10 * 60_000)?;
            } else {
                let action = nothingtodo_core::alarms::NotificationAction::Complete;
                store.apply_notification_action(&ringing.task_id, ringing.at, &action, now)?;
            }
        }
        state.edited();
        let _ = app.emit("tasks-changed", ());
        Ok(())
    }
}
