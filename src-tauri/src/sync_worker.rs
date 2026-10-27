use crate::AppState;
use nothingtodo_core::schedule::SyncSchedule;
use serde::Serialize;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{Emitter, Manager};
use tokio::sync::mpsc;

pub enum Trigger {
    Edited,
    Wake,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub connected: bool,
    pub syncing: bool,
    pub last_synced_at: Option<u64>,
    pub error: Option<String>,
    pub retry_in_seconds: Option<u64>,
}

async fn publish(app: &tauri::AppHandle, status: SyncStatus) {
    *app.state::<AppState>().sync_status.lock().await = status.clone();
    let _ = app.emit("sync-status", status);
}

/// One worker owns all network sync attempts, including manual requests.
pub async fn run(app: tauri::AppHandle, mut receiver: mpsc::Receiver<Trigger>) {
    let origin = Instant::now();
    let mut schedule = SyncSchedule::default();
    let mut status = SyncStatus::default();
    loop {
        let delay = schedule.delay(origin.elapsed());
        tokio::select! {
            // Drain queued edits before syncing, to avoid redundant immediate attempts.
            biased;
            trigger = receiver.recv() => {
                let Some(trigger) = trigger else { return; };
                if app.state::<AppState>().sync.lock().await.is_none() { continue; }
                match trigger {
                    Trigger::Edited => schedule.edited(origin.elapsed()),
                    Trigger::Wake => schedule.wake(origin.elapsed()),
                }
                continue;
            }
            _ = async {
                match delay {
                    Some(delay) => tokio::time::sleep(delay).await,
                    None => std::future::pending::<()>().await,
                }
            } => {}
        }
        let state = app.state::<AppState>();
        let client = state.sync.lock().await;
        let Some(client) = client.as_ref() else {
            continue;
        };
        status.connected = true;
        status.syncing = true;
        status.retry_in_seconds = None;
        publish(&app, status.clone()).await;
        // Preserve the existing serialization between sync acknowledgements and edits.
        let result = client.sync(&mut *state.store.lock().await).await;
        status.syncing = false;
        match result {
            Ok(()) => {
                schedule.succeeded(origin.elapsed());
                status.error = None;
                status.last_synced_at = Some(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or(Duration::ZERO)
                        .as_millis() as u64,
                );
            }
            Err(error) => {
                status.error = Some(error);
                status.retry_in_seconds = Some(schedule.failed(origin.elapsed()));
            }
        }
        publish(&app, status.clone()).await;
        let _ = app.emit("tasks-changed", ());
    }
}
