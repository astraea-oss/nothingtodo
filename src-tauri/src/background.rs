//! Android background sync. `SyncJob.kt` runs this every 15 minutes or so while the phone has a
//! network, including when the app has not been opened for days: it applies notification button
//! presses, syncs, and hands back the reminder list to arm, so a change on another device reaches
//! the phone's native alarms without opening the app.
//!
//! It may run in the app's process, before or while the app starts. Both use one `Store` (see
//! `shared_store`) and one sync pass at a time (`sync_worker::NETWORK`). When the app is running,
//! its own worker does the sync.
use crate::{
    AppState, credentials,
    reminders::{Actions, apply_pressed, now_ms, schedule_json},
    sync_worker::{NETWORK, Trigger},
};
use jni::{
    Env, EnvUnowned,
    objects::{JObject, JString},
};
use nothingtodo_core::{Store, sync::Outcome, sync::SyncClient};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex, OnceLock},
    time::Duration,
};
use tauri::{AppHandle, Manager};
use tokio::sync::Mutex;

/// Set once the app's `setup` has run in this process.
static APP: OnceLock<AppHandle> = OnceLock::new();
/// Credentials read by `prepare`, for the following `run` only.
static CREDENTIALS: StdMutex<Option<Result<Option<credentials::Saved>, String>>> =
    StdMutex::new(None);

/// Long histories download in bounded passes; a background run takes a few at most.
const MAX_PASSES: usize = 8;

pub fn app_started(app: &AppHandle) {
    let _ = APP.set(app.clone());
}

fn answer<'local>(
    env: &mut Env<'local>,
    result: Result<serde_json::Value, String>,
) -> JString<'local> {
    let body = result.unwrap_or_else(|error| json!({ "error": error }));
    JString::from_str(env, body.to_string()).unwrap_or_default()
}

/// Called by `SyncJob` with its lock held, and only when no activity has started in this
/// process. The keyring and HTTPS checks need an Android context; the app's activity normally
/// publishes one through `ndk-context`, and it asserts that nobody did so before it. So it is
/// published here only while the keyring opens, then withdrawn again under the same lock.
#[unsafe(no_mangle)]
pub extern "system" fn Java_uk_co_astraeaoss_nothingtodo_BackgroundSync_prepare<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    context: JObject<'caller>,
    package: JString<'caller>,
) {
    // jni-rs is only usable inside `with_env`, string conversion included.
    let outcome = unowned_env.with_env(|env| -> Result<(), jni::errors::Error> {
        let package = package.to_string();
        let loaded = (|| {
            // SAFETY: a second wrapper around the caller's local reference, used within this call.
            let verifier_context = unsafe { JObject::from_raw(env, context.as_raw()) };
            rustls_platform_verifier::android::init_with_env(env, verifier_context)
                .map_err(|e| format!("Could not set up HTTPS certificate checks: {e}"))?;
            let vm = env.get_java_vm().map_err(|e| e.to_string())?;
            // SAFETY: the VM pointer is valid for the life of the process and the context
            // reference for this call, which is as long as the context stays published.
            unsafe {
                ndk_context::initialize_android_context(
                    vm.get_raw().cast(),
                    context.as_raw().cast(),
                )
            };
            let loaded = credentials::load_now(&crate::keyring_service_for(&package));
            // SAFETY: published just above, and nothing else publishes while the lock is held.
            unsafe { ndk_context::release_android_context() };
            loaded
        })();
        if let Ok(mut slot) = CREDENTIALS.lock() {
            *slot = Some(loaded);
        }
        Ok(())
    });
    outcome.resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

/// One background pass. `actions` is `Reminders.actions` as JSON. Returns
/// `{"reminders": [...], "acknowledge": [...], "synced": bool, "syncError": "..."?}`, or
/// `{"error": "..."}` when nothing could be done.
#[unsafe(no_mangle)]
pub extern "system" fn Java_uk_co_astraeaoss_nothingtodo_BackgroundSync_run<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    data_dir: JString<'caller>,
    actions: JString<'caller>,
) -> JString<'caller> {
    let outcome = unowned_env.with_env(|env| -> Result<_, jni::errors::Error> {
        let data_dir = PathBuf::from(data_dir.to_string());
        let actions = actions.to_string();
        let result = tauri::async_runtime::block_on(pass(data_dir, actions));
        Ok(answer(env, result))
    });
    outcome.resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

async fn pass(data_dir: PathBuf, actions: String) -> Result<serde_json::Value, String> {
    let pressed: Actions =
        serde_json::from_str(&format!("{{\"actions\":{actions}}}")).map_err(|e| e.to_string())?;
    let credentials = CREDENTIALS.lock().ok().and_then(|mut slot| slot.take());
    // The app may be starting right now; give its setup a moment to finish.
    let mut app = APP.get();
    if credentials.is_none() {
        for _ in 0..40 {
            if app.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
            app = APP.get();
        }
    }
    let store = match app {
        Some(app) => app.state::<AppState>().store.clone(),
        None => crate::shared_store(data_dir.join("nothingtodo.sqlite3"))?,
    };
    let (changed, acknowledge) = apply_pressed(&mut *store.lock().await, &pressed)?;
    // A failed sync still re-arms from what this device knows.
    let synced = match app {
        Some(app) => Ok(sync_in_app(app, changed).await),
        None => match credentials {
            Some(Ok(Some(saved))) => sync_headless(&store, saved).await,
            Some(Ok(None)) | None => Ok(false),
            Some(Err(error)) => Err(error),
        },
    };
    let alarms = store.lock().await.alarms(now_ms())?;
    Ok(json!({
        "reminders": schedule_json(&alarms),
        "acknowledge": acknowledge,
        "synced": synced.as_ref().is_ok_and(|s| *s),
        "syncError": synced.err(),
    }))
}

/// The app's worker owns sync while the app runs: wake it and wait for its pass to end.
async fn sync_in_app(app: &AppHandle, changed: bool) -> bool {
    let state = app.state::<AppState>();
    if state.sync.lock().await.is_none() {
        return false;
    }
    let before = state.sync_status.lock().await.last_synced_at;
    let trigger = if changed {
        Trigger::Edited
    } else {
        Trigger::Wake
    };
    if state.sync_trigger.send(trigger).await.is_err() {
        return false;
    }
    for _ in 0..120 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let status = state.sync_status.lock().await;
        if status.last_synced_at != before {
            return true;
        }
        if status.error.is_some() && !status.syncing {
            return false;
        }
    }
    false
}

async fn sync_headless(
    store: &Arc<Mutex<Store>>,
    saved: credentials::Saved,
) -> Result<bool, String> {
    let client = SyncClient::new(&saved.endpoint, saved.token)?;
    let _pass = NETWORK.lock().await;
    for _ in 0..MAX_PASSES {
        if client.sync(store).await? == Outcome::Complete {
            return Ok(true);
        }
    }
    Ok(false)
}
