//! Backup commands. Files are plaintext and contain no credentials. Linux saves into the portable
//! data folder; Android saves to Downloads.
use crate::{AppState, reminders};
use nothingtodo_core::backup::{self, Summary};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

/// `stamp` comes from the page so file names use local time; keep only safe characters.
fn file_name(prefix: &str, stamp: &str) -> String {
    let stamp: String = stamp
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(32)
        .collect();
    format!("{prefix}-{stamp}.json")
}

#[cfg(not(target_os = "android"))]
fn save(app: &AppHandle, name: &str, text: &str) -> Result<String, String> {
    let folder = app.state::<AppState>().data_dir.join("backups");
    std::fs::create_dir_all(&folder)
        .map_err(|e| format!("Could not create {}: {e}", folder.display()))?;
    let path = folder.join(name);
    std::fs::write(&path, text).map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    Ok(path.display().to_string())
}

#[cfg(target_os = "android")]
fn save(app: &AppHandle, name: &str, text: &str) -> Result<String, String> {
    reminders::save_to_downloads(app, name, text)
}

async fn export_to(app: &AppHandle, prefix: &str, stamp: &str) -> Result<String, String> {
    let json = app
        .state::<AppState>()
        .store
        .lock()
        .await
        .export(reminders::now_ms())?;
    let app = app.clone();
    let name = file_name(prefix, stamp);
    tauri::async_runtime::spawn_blocking(move || save(&app, &name, &json))
        .await
        .map_err(|_| "Saving the backup stopped unexpectedly.".to_string())?
}

/// Returns where the file was saved.
#[tauri::command]
pub async fn export_backup(app: AppHandle, stamp: String) -> Result<String, String> {
    export_to(&app, "nothingtodo-backup", &stamp).await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    #[serde(flatten)]
    backup: Summary,
    current_tasks: usize,
}

/// Checks a chosen file and describes it, without changing anything.
#[tauri::command]
pub async fn preview_backup(state: State<'_, AppState>, json: String) -> Result<Preview, String> {
    let parsed = backup::parse(&json)?;
    let mut scratch = nothingtodo_core::Store::open(":memory:")?;
    let backup = scratch.restore(parsed)?;
    let current_tasks = state
        .store
        .lock()
        .await
        .snapshot()
        .records
        .values()
        .filter(|r| r.task.is_some())
        .count();
    Ok(Preview {
        backup,
        current_tasks,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Restored {
    #[serde(flatten)]
    summary: Summary,
    /// Where the data that was replaced was saved first.
    previous: String,
}

/// Replaces this profile with the backup, after saving the current data.
#[tauri::command]
pub async fn restore_backup(
    app: AppHandle,
    json: String,
    stamp: String,
) -> Result<Restored, String> {
    let parsed = backup::parse(&json)?;
    let previous = export_to(&app, "nothingtodo-before-restore", &stamp).await?;
    let state = app.state::<AppState>();
    let summary = state.store.lock().await.restore(parsed)?;
    state.edited();
    let _ = app.emit("tasks-changed", ());
    Ok(Restored { summary, previous })
}

/// Adds the backup's tasks as new tasks.
#[tauri::command]
pub async fn copy_backup(app: AppHandle, json: String) -> Result<Summary, String> {
    let parsed = backup::parse(&json)?;
    let state = app.state::<AppState>();
    let summary = state.store.lock().await.copy_from(parsed)?;
    state.edited();
    let _ = app.emit("tasks-changed", ());
    Ok(summary)
}
