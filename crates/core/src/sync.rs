use crate::{Record, Remote, Result, Snapshot, Store};
use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::time::Duration;

const APP: &str = "nothingtodo";

pub struct SyncClient {
    client: Client,
    base: Url,
    token: String,
    binding: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    changes: Vec<Remote>,
    cursor: u64,
    has_more: bool,
}
#[derive(Deserialize)]
struct Setting {
    setting: Remote,
}
#[derive(Deserialize)]
struct Conflict {
    current: Option<Remote>,
}

impl SyncClient {
    /// Credentials live only in Rust memory for this session. Redirects are disabled.
    pub fn new(endpoint: &str, token: String) -> Result<Self> {
        let base = Url::parse(endpoint.trim_end_matches('/')).map_err(|_| "Invalid API URL.")?;
        let local = matches!(base.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if !(base.scheme() == "https" || (local && base.scheme() == "http"))
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || base.path() != "/"
        {
            return Err("Use an HTTPS origin, or HTTP localhost for development.".into());
        }
        if token.len() != 46
            || !token.starts_with("as_")
            || !token[3..]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err("Enter a valid Astraea account token.".into());
        }
        // Prevent accidentally uploading an existing local profile to another account/server.
        // Token rotation needs an explicit account migration flow in a later milestone.
        let binding = format!(
            "{:x}",
            Sha256::digest(format!("{}\n{token}", base.as_str()))
        );
        let client = Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "Could not initialize HTTP client.")?;
        Ok(Self {
            client,
            base,
            token,
            binding,
        })
    }

    fn url(&self, resource: &str) -> Result<Url> {
        self.base
            .join(&format!("v1/apps/{APP}/{resource}"))
            .map_err(|_| "Invalid sync URL.".into())
    }

    pub async fn sync(&self, store: &mut Store) -> Result<()> {
        if store
            .state
            .binding
            .as_ref()
            .is_some_and(|b| b != &self.binding)
        {
            return Err("This local profile is bound to another token or server. Use the original credentials; account migration is not implemented yet.".into());
        }
        // Pull first; persist each applied page and its cursor together.
        loop {
            let response = self
                .client
                .get(self.url("changes")?)
                .query(&[("after", store.state.cursor)])
                .bearer_auth(&self.token)
                .send()
                .await
                .map_err(|_| "Sync connection failed. Local changes are safe.")?;
            let response = checked(response)?;
            let page: Page = response
                .json()
                .await
                .map_err(|_| "Invalid change feed response.")?;
            if page.cursor < store.state.cursor
                || (page.has_more && page.cursor == store.state.cursor)
            {
                return Err("Invalid change feed cursor.".into());
            }
            let mut state = store.snapshot();
            for remote in page.changes {
                apply(&mut state, remote)?;
            }
            state.cursor = page.cursor;
            state.binding = Some(self.binding.clone());
            store.commit(state)?;
            if !page.has_more {
                break;
            }
        }
        let mut state = store.snapshot();
        reconcile_identical(&mut state);
        store.commit(state)?;
        let pending: Vec<_> = store
            .state
            .records
            .iter()
            .filter(|(_, r)| r.dirty && r.conflict.is_none())
            .map(|(id, r)| (id.clone(), r.clone()))
            .collect();
        for (id, record) in pending {
            // A locally created and deleted item has never existed remotely.
            if record.version == 0 && record.task.is_none() {
                let mut state = store.snapshot();
                state.records.remove(&id);
                store.commit(state)?;
                continue;
            }
            let url = self.url(&format!("settings/{id}"))?;
            let body = serde_json::json!({ "baseVersion": record.version, "value": record.task });
            let request = if record.task.is_some() {
                self.client.put(url)
            } else {
                self.client.delete(url)
            };
            let response = request
                .bearer_auth(&self.token)
                .json(&body)
                .send()
                .await
                .map_err(|_| {
                    "Sync write failed. Retry sync to reconcile the server state.".to_string()
                })?;
            let mut state = store.snapshot();
            if response.status() == StatusCode::CONFLICT {
                let conflict: Conflict = response
                    .json()
                    .await
                    .map_err(|_| "Invalid conflict response.")?;
                match conflict.current {
                    Some(remote) => apply(&mut state, remote)?,
                    None => {
                        return Err("Server record is missing; local changes were retained.".into());
                    }
                }
            } else {
                let setting: Setting = checked(response)?
                    .json()
                    .await
                    .map_err(|_| "Invalid write response.")?;
                if setting.setting.key != id {
                    return Err("Server returned a different task key.".into());
                }
                apply(&mut state, setting.setting)?;
            }
            reconcile_identical(&mut state);
            store.commit(state)?;
        }
        Ok(())
    }
}

fn checked(response: reqwest::Response) -> Result<reqwest::Response> {
    if response.status().is_success() {
        Ok(response)
    } else {
        Err(format!(
            "Sync returned HTTP {}. Local changes are safe.",
            response.status().as_u16()
        ))
    }
}

fn apply(state: &mut Snapshot, remote: Remote) -> Result<()> {
    // Only UUID task keys are owned by this version of the app.
    if uuid::Uuid::parse_str(&remote.key).is_err() {
        return Err("Unsupported remote task key.".into());
    }
    if remote.version == 0 || remote.deleted != remote.value.is_none() {
        return Err("Invalid remote record.".into());
    }
    if let Some(task) = &remote.value {
        task.validate()?;
    }
    let record = state.records.entry(remote.key.clone()).or_insert(Record {
        task: None,
        version: 0,
        dirty: false,
        conflict: None,
        notified_at: None,
    });
    if remote.version <= record.version {
        return Ok(());
    }
    if record.dirty {
        // Keep the local base version until the user explicitly chooses a side.
        if record
            .conflict
            .as_ref()
            .is_none_or(|old| old.version < remote.version)
        {
            record.conflict = Some(remote);
        }
    } else {
        if record.task != remote.value {
            record.notified_at = None;
        }
        record.task = remote.value;
        record.version = remote.version;
        record.dirty = false;
        record.conflict = None;
    }
    Ok(())
}

// Compare against the latest event only, after the entire feed has been replayed.
// An older event matching a pending edit must never acknowledge that edit.
fn reconcile_identical(state: &mut Snapshot) {
    for record in state.records.values_mut() {
        if let Some(remote) = &record.conflict
            && record.task == remote.value
        {
            record.version = remote.version;
            record.dirty = false;
            record.conflict = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Task;
    fn task(title: &str) -> Task {
        Task {
            schema_version: 1,
            title: title.into(),
            notes: "".into(),
            completed: false,
            remind_at: None,
        }
    }
    fn remote(key: &str, title: Option<&str>, version: u64) -> Remote {
        Remote {
            key: key.into(),
            value: title.map(task),
            version,
            deleted: title.is_none(),
        }
    }

    #[test]
    fn conflict_preserves_local_and_can_choose_either_side() {
        for keep_local in [true, false] {
            let mut store = Store::open(":memory:").unwrap();
            let id = uuid::Uuid::new_v4().to_string();
            apply(&mut store.state, remote(&id, Some("Original"), 1)).unwrap();
            store.save_task(Some(id.clone()), task("Local")).unwrap();
            apply(&mut store.state, remote(&id, Some("Remote"), 2)).unwrap();
            assert_eq!(
                store.state.records[&id].task.as_ref().unwrap().title,
                "Local"
            );
            assert_eq!(store.state.records[&id].version, 1);
            store.resolve(&id, keep_local).unwrap();
            assert_eq!(store.state.records[&id].dirty, keep_local);
            assert_eq!(store.state.records[&id].version, 2);
            assert_eq!(
                store.state.records[&id].task.as_ref().unwrap().title,
                if keep_local { "Local" } else { "Remote" }
            );
        }
    }

    #[test]
    fn lost_write_acknowledgement_is_reconciled_and_deletions_do_not_resurrect() {
        let mut store = Store::open(":memory:").unwrap();
        let id = store.save_task(None, task("Local")).unwrap();
        apply(&mut store.state, remote(&id, Some("Local"), 1)).unwrap();
        reconcile_identical(&mut store.state);
        assert!(!store.state.records[&id].dirty);
        store.delete_task(&id).unwrap();
        apply(&mut store.state, remote(&id, None, 2)).unwrap();
        reconcile_identical(&mut store.state);
        assert!(!store.state.records[&id].dirty);
        apply(&mut store.state, remote(&id, Some("Old event"), 1)).unwrap();
        assert!(store.state.records[&id].task.is_none());
    }

    #[test]
    fn historical_match_cannot_acknowledge_a_pending_edit() {
        let mut store = Store::open(":memory:").unwrap();
        let id = store.save_task(None, task("Local")).unwrap();
        apply(&mut store.state, remote(&id, Some("Local"), 1)).unwrap();
        apply(&mut store.state, remote(&id, Some("Newer remote"), 2)).unwrap();
        reconcile_identical(&mut store.state);
        assert!(store.state.records[&id].dirty);
        assert_eq!(
            store.state.records[&id].task.as_ref().unwrap().title,
            "Local"
        );
        assert_eq!(
            store.state.records[&id].conflict.as_ref().unwrap().version,
            2
        );
    }

    #[test]
    fn invalid_remote_page_does_not_advance_persisted_cursor() {
        let store = Store::open(":memory:").unwrap();
        let mut next = store.snapshot();
        assert!(apply(&mut next, remote("not-a-uuid", Some("Bad"), 1)).is_err());
        assert_eq!(store.snapshot().cursor, 0);
        assert!(store.snapshot().records.is_empty());
    }

    #[test]
    fn rejects_insecure_remote_origins_and_credential_urls() {
        let token = format!("as_{}", "a".repeat(43));
        for url in [
            "http://example.com",
            "https://user@example.com",
            "https://example.com/path",
            "https://example.com?token=x",
        ] {
            assert!(SyncClient::new(url, token.clone()).is_err());
        }
        assert!(SyncClient::new("http://localhost:8787", token).is_ok());
    }
}
