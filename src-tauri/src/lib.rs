use nothingtodo_core::{Snapshot, Store, Task, sync::SyncClient};
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};
use tokio::sync::{Mutex, mpsc};

#[cfg(target_os = "android")]
mod android;
mod credentials;
mod paths;
mod reminders;
mod sync_worker;
mod window;
use sync_worker::{SyncStatus, Trigger, update};

struct AppState {
    store: Mutex<Store>,
    sync: Mutex<Option<Arc<SyncClient>>>,
    sync_status: Mutex<SyncStatus>,
    sync_trigger: mpsc::Sender<Trigger>,
    reminder_trigger: mpsc::Sender<()>,
}

impl AppState {
    /// After a local edit: upload it soon and bring scheduled reminders up to date.
    fn edited(&self) {
        let _ = self.sync_trigger.try_send(Trigger::Edited);
        let _ = self.reminder_trigger.try_send(());
    }
}

#[tauri::command]
async fn snapshot(state: State<'_, AppState>) -> Result<Snapshot, String> {
    Ok(state.store.lock().await.snapshot())
}

#[tauri::command]
async fn save_task(
    state: State<'_, AppState>,
    id: Option<String>,
    task: Task,
) -> Result<(), String> {
    state.store.lock().await.save_task(id, task)?;
    state.edited();
    Ok(())
}

/// Device-local: moves this device's next notification without changing the synced task.
#[tauri::command]
async fn snooze_task(state: State<'_, AppState>, id: String, minutes: i64) -> Result<(), String> {
    if !(1..=24 * 60).contains(&minutes) {
        return Err("Snooze for between 1 minute and 24 hours.".into());
    }
    let now = reminders::now_ms();
    state
        .store
        .lock()
        .await
        .snooze(&id, now, now + minutes * 60_000)?;
    // Snoozes are never uploaded, so only the reminder schedule changes.
    let _ = state.reminder_trigger.try_send(());
    Ok(())
}

#[tauri::command]
async fn delete_task(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.store.lock().await.delete_task(&id)?;
    state.edited();
    Ok(())
}

#[tauri::command]
async fn resolve_conflict(
    state: State<'_, AppState>,
    id: String,
    keep_local: bool,
) -> Result<(), String> {
    state.store.lock().await.resolve(&id, keep_local)?;
    state.edited();
    Ok(())
}

/// Replaces the sync session. Lock order is always store, then session; nothing holds the
/// session lock while waiting for the store. Holding the store lock while cancelling ensures no
/// response from the previous session is committed after this returns.
async fn install(app: &AppHandle, client: Option<SyncClient>) {
    let state = app.state::<AppState>();
    let _store = state.store.lock().await;
    let mut sync = state.sync.lock().await;
    if let Some(previous) = sync.take() {
        previous.cancel();
    }
    *sync = client.map(Arc::new);
}

fn keyring_service(app: &AppHandle) -> String {
    app.config().identifier.clone()
}

#[tauri::command]
async fn connect_sync(
    app: AppHandle,
    state: State<'_, AppState>,
    endpoint: String,
    token: String,
    remember: bool,
) -> Result<(), String> {
    let client = SyncClient::new(&endpoint, token.clone())?;
    install(&app, Some(client)).await;
    let service = keyring_service(&app);
    let stored = if remember {
        credentials::save(service, credentials::Saved { endpoint, token }).await
    } else {
        credentials::forget(service).await
    };
    update(&app, |s| {
        s.connected = true;
        s.error = None;
        s.remembered = remember && stored.is_ok();
        s.notice = stored
            .err()
            .map(|e| format!("{e} Sync is connected for this session only."));
    })
    .await;
    state
        .sync_trigger
        .send(Trigger::Wake)
        .await
        .map_err(|_| "Sync worker stopped.".to_string())
}

#[tauri::command]
async fn disconnect_sync(app: AppHandle) -> Result<(), String> {
    install(&app, None).await;
    let forgotten = credentials::forget(keyring_service(&app)).await;
    update(&app, |s| {
        *s = SyncStatus {
            last_synced_at: s.last_synced_at,
            notice: forgotten.as_ref().err().cloned(),
            ..SyncStatus::default()
        };
    })
    .await;
    forgotten
}

/// Reconnects with credentials remembered in the system keyring, if any.
async fn restore_session(app: AppHandle) {
    let notice = match credentials::load(keyring_service(&app)).await {
        Ok(None) => return,
        Ok(Some(saved)) => match SyncClient::new(&saved.endpoint, saved.token) {
            Ok(client) => {
                install(&app, Some(client)).await;
                update(&app, |s| {
                    s.connected = true;
                    s.remembered = true;
                })
                .await;
                let _ = app
                    .state::<AppState>()
                    .sync_trigger
                    .send(Trigger::Wake)
                    .await;
                return;
            }
            Err(e) => format!("Saved sync credentials are invalid: {e}"),
        },
        Err(e) => format!("{e} Connect for this session instead."),
    };
    update(&app, |s| s.notice = Some(notice)).await;
}

#[tauri::command]
async fn sync_now(state: State<'_, AppState>) -> Result<(), String> {
    if state.sync.lock().await.is_none() {
        return Err("Connect sync for this session first.".into());
    }
    state
        .sync_trigger
        .send(Trigger::Wake)
        .await
        .map_err(|_| "Sync worker stopped.".to_string())
}

#[tauri::command]
async fn sync_status(state: State<'_, AppState>) -> Result<SyncStatus, String> {
    Ok(state.sync_status.lock().await.clone())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    // Must be registered first. A second launch shows the existing window instead of opening
    // the same profile twice.
    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            window::show_main(app)
        }))
        .on_window_event(window::on_event);
    let builder = builder.plugin(tauri_plugin_notification::init());
    #[cfg(target_os = "android")]
    let builder = builder.plugin(reminders::plugin());
    builder
        .setup(|app| {
            #[cfg(target_os = "android")]
            android::init_tls().map_err(std::io::Error::other)?;
            let paths = paths::AppPaths::resolve(app.handle()).map_err(std::io::Error::other)?;
            let store = Store::open(paths.database()).map_err(std::io::Error::other)?;
            let (sync_trigger, receiver) = mpsc::channel(16);
            let (reminder_trigger, reminder_requests) = mpsc::channel(1);
            app.manage(AppState {
                store: Mutex::new(store),
                sync: Mutex::new(None),
                sync_status: Mutex::new(SyncStatus::default()),
                sync_trigger,
                reminder_trigger,
            });
            tauri::async_runtime::spawn(sync_worker::run(app.handle().clone(), receiver));
            tauri::async_runtime::spawn(reminders::run(app.handle().clone(), reminder_requests));
            reminders::request(app.handle());
            tauri::async_runtime::spawn(restore_session(app.handle().clone()));
            #[cfg(desktop)]
            let tray = window::create_tray(app.handle());
            #[cfg(desktop)]
            if let Err(error) = &tray {
                eprintln!("NothingToDo tray unavailable; closing the window will quit: {error}");
            }
            #[cfg(desktop)]
            let visible = tray.is_err() || !window::start_hidden();
            #[cfg(mobile)]
            let visible = true;
            window::create_main(app.handle(), &paths, visible)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            snapshot,
            save_task,
            snooze_task,
            delete_task,
            resolve_conflict,
            connect_sync,
            disconnect_sync,
            sync_now,
            sync_status
        ])
        .run(tauri::generate_context!())
        .expect("error while running NothingToDo");
}
