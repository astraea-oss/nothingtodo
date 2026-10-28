use crate::AppState;
use nothingtodo_core::{schedule::SyncSchedule, sync::Outcome};
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
    /// Credentials are in the system keyring and reconnect after a restart.
    pub remembered: bool,
    pub syncing: bool,
    /// A long change history is still downloading in bounded passes.
    pub catching_up: bool,
    pub last_synced_at: Option<u64>,
    pub error: Option<String>,
    pub retry_in_seconds: Option<u64>,
    /// Credential storage problems, shown without interrupting sync.
    pub notice: Option<String>,
}

/// Updates the shared status and notifies the UI.
pub async fn update(app: &tauri::AppHandle, change: impl FnOnce(&mut SyncStatus)) {
    let state = app.state::<AppState>();
    let status = {
        let mut status = state.sync_status.lock().await;
        change(&mut status);
        status.clone()
    };
    let _ = app.emit("sync-status", status);
}

/// One worker owns all network sync attempts, including manual requests.
pub async fn run(app: tauri::AppHandle, mut receiver: mpsc::Receiver<Trigger>) {
    let origin = Instant::now();
    let mut schedule = SyncSchedule::default();
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
        // Hold the session lock only long enough to take a reference, so connecting or
        // disconnecting never waits for a network request.
        let Some(client) = state.sync.lock().await.clone() else {
            schedule = SyncSchedule::default();
            continue;
        };
        update(&app, |s| {
            s.syncing = true;
            s.retry_in_seconds = None;
        })
        .await;
        // The store lock is taken only for short commits inside `sync`.
        let result = client.sync(&state.store).await;
        if client.is_cancelled() {
            // A replaced or disconnected session; its result says nothing about the new one.
            update(&app, |s| s.syncing = false).await;
            schedule.wake(origin.elapsed());
            continue;
        }
        let elapsed = origin.elapsed();
        let retry = match &result {
            Err(_) => Some(schedule.failed(elapsed)),
            Ok(Outcome::MoreHistory { .. }) => {
                schedule.wake(elapsed);
                None
            }
            Ok(Outcome::Complete) => {
                schedule.succeeded(elapsed);
                None
            }
        };
        update(&app, |s| {
            s.syncing = false;
            s.retry_in_seconds = retry;
            match result {
                Ok(outcome) => {
                    s.error = None;
                    s.catching_up = matches!(outcome, Outcome::MoreHistory { .. });
                    if outcome == Outcome::Complete {
                        s.last_synced_at = Some(
                            SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or(Duration::ZERO)
                                .as_millis() as u64,
                        );
                    }
                }
                Err(error) => s.error = Some(error),
            }
        })
        .await;
        let _ = app.emit("tasks-changed", ());
    }
}
