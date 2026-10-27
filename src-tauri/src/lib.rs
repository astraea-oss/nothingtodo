use nothingtodo_core::{Snapshot, Store, Task, sync::SyncClient};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{Emitter, Manager, State};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::{Mutex, mpsc};

mod sync_worker;
use sync_worker::{SyncStatus, Trigger};

struct AppState {
    store: Mutex<Store>,
    sync: Mutex<Option<SyncClient>>,
    sync_status: Mutex<SyncStatus>,
    sync_trigger: mpsc::Sender<Trigger>,
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
    let _ = state.sync_trigger.try_send(Trigger::Edited);
    Ok(())
}

#[tauri::command]
async fn delete_task(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.store.lock().await.delete_task(&id)?;
    let _ = state.sync_trigger.try_send(Trigger::Edited);
    Ok(())
}

#[tauri::command]
async fn resolve_conflict(
    state: State<'_, AppState>,
    id: String,
    keep_local: bool,
) -> Result<(), String> {
    state.store.lock().await.resolve(&id, keep_local)?;
    let _ = state.sync_trigger.try_send(Trigger::Edited);
    Ok(())
}

#[tauri::command]
async fn connect_sync(
    state: State<'_, AppState>,
    endpoint: String,
    token: String,
) -> Result<(), String> {
    *state.sync.lock().await = Some(SyncClient::new(&endpoint, token)?);
    state
        .sync_trigger
        .send(Trigger::Wake)
        .await
        .map_err(|_| "Sync worker stopped.".to_string())?;
    Ok(())
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
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let store =
                Store::open(dir.join("nothingtodo.sqlite3")).map_err(std::io::Error::other)?;
            let (sync_trigger, receiver) = mpsc::channel(16);
            app.manage(AppState {
                store: Mutex::new(store),
                sync: Mutex::new(None),
                sync_status: Mutex::new(SyncStatus::default()),
                sync_trigger,
            });
            tauri::async_runtime::spawn(sync_worker::run(app.handle().clone(), receiver));
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    let state = handle.state::<AppState>();
                    let mut store = state.store.lock().await;
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as i64;
                    if handle.notification().permission_state().ok()
                        != Some(tauri_plugin_notification::PermissionState::Granted)
                    {
                        continue;
                    }
                    for (id, task) in store.due(now) {
                        // Mobile suspension/termination requires native OS scheduling, a later milestone.
                        let Some(due) = task.remind_at else {
                            continue;
                        };
                        if handle
                            .notification()
                            .builder()
                            .title("NothingToDo reminder")
                            .body(&task.title)
                            .show()
                            .is_err()
                        {
                            continue;
                        }
                        if let Err(error) = store.mark_notified(&id, due) {
                            let _ = handle.emit("reminder-error", error);
                        }
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            snapshot,
            save_task,
            delete_task,
            resolve_conflict,
            connect_sync,
            sync_now,
            sync_status
        ])
        .run(tauri::generate_context!())
        .expect("error while running NothingToDo");
}
