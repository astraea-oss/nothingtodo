//! Protected sync credentials. Linux uses the Secret Service (for example GNOME Keyring or
//! KWallet); Android uses SharedPreferences encrypted with a key held in the Android Keystore.
//! Other platforms stay session-only. Tokens are never written to SQLite, frontend storage or
//! logs.
use serde::{Deserialize, Serialize};

const ACCOUNT: &str = "astraea-sync";

#[derive(Serialize, Deserialize)]
pub struct Saved {
    pub endpoint: String,
    pub token: String,
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod platform {
    use super::{ACCOUNT, Saved};
    use keyring_core::{Entry, Error};
    use std::sync::LazyLock;

    static STORE: LazyLock<Result<(), String>> = LazyLock::new(|| {
        #[cfg(target_os = "linux")]
        let store = zbus_secret_service_keyring_store::Store::new();
        // Needs the Android context, which Tauri initialises before any command runs.
        #[cfg(target_os = "android")]
        let store = android_native_keyring_store::Store::new();
        keyring_core::set_default_store(
            store.map_err(|e| format!("No usable system keyring: {e}"))?,
        );
        Ok(())
    });

    fn entry(service: &str) -> Result<Entry, String> {
        STORE.clone()?;
        Entry::new(service, ACCOUNT).map_err(|e| e.to_string())
    }

    pub fn load(service: &str) -> Result<Option<Saved>, String> {
        match entry(service)?.get_password() {
            Ok(json) => serde_json::from_str(&json)
                .map(Some)
                .map_err(|_| "Saved sync credentials are unreadable; connect again.".into()),
            Err(Error::NoEntry) => Ok(None),
            Err(e) => Err(format!("Could not read the system keyring: {e}")),
        }
    }

    pub fn save(service: &str, saved: &Saved) -> Result<(), String> {
        let json = serde_json::to_string(saved).map_err(|e| e.to_string())?;
        entry(service)?
            .set_password(&json)
            .map_err(|e| format!("Could not save to the system keyring: {e}"))
    }

    pub fn forget(service: &str) -> Result<(), String> {
        match entry(service)?.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(e) => Err(format!("Could not remove saved credentials: {e}")),
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
mod platform {
    use super::Saved;

    pub fn load(_: &str) -> Result<Option<Saved>, String> {
        Ok(None)
    }

    pub fn save(_: &str, _: &Saved) -> Result<(), String> {
        Err("Protected credential storage is not available on this platform yet.".into())
    }

    pub fn forget(_: &str) -> Result<(), String> {
        Ok(())
    }
}

/// Keyring calls can block on D-Bus or an unlock prompt, so run them off the async threads.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|_| "Credential storage stopped unexpectedly.".to_string())?
}

pub async fn load(service: String) -> Result<Option<Saved>, String> {
    blocking(move || platform::load(&service)).await
}

pub async fn save(service: String, saved: Saved) -> Result<(), String> {
    blocking(move || platform::save(&service, &saved)).await
}

pub async fn forget(service: String) -> Result<(), String> {
    blocking(move || platform::forget(&service)).await
}
