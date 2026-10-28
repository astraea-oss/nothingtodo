use crate::{Remote, Result, Snapshot, Store, Task};
use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;

const APP: &str = "nothingtodo";
/// Bounds one attempt so a long first download yields to the worker's schedule between passes.
const PAGES_PER_PASS: usize = 20;

pub struct SyncClient {
    client: Client,
    base: Url,
    token: String,
    binding: String,
    cancelled: AtomicBool,
    pages_per_pass: usize,
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// Feed replayed and pending edits uploaded.
    Complete,
    /// More history remains; call again soon. Pending edits wait for the full replay.
    MoreHistory { cursor: u64 },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Page {
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

/// A pending local change captured for upload while the store lock is released.
#[derive(Clone, Debug)]
pub(crate) struct Upload {
    id: String,
    task: Option<Task>,
    base_version: u64,
    local_revision: u64,
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
            cancelled: AtomicBool::new(false),
            pages_per_pass: PAGES_PER_PASS,
        })
    }

    /// Changes how many feed pages one `sync` call applies before returning.
    pub fn with_pages_per_pass(mut self, pages: usize) -> Self {
        self.pages_per_pass = pages.max(1);
        self
    }

    /// Stops this session from committing anything further. Call while holding the store lock
    /// so no response from the replaced session can be applied afterwards.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn live(&self) -> Result<()> {
        if self.is_cancelled() {
            return Err("The sync session was replaced; its responses were discarded.".into());
        }
        Ok(())
    }

    fn url(&self, resource: &str) -> Result<Url> {
        self.base
            .join(&format!("v1/apps/{APP}/{resource}"))
            .map_err(|_| "Invalid sync URL.".into())
    }

    /// The store lock is held only to read or commit state, never across a network request.
    /// Network attempts must still be serialized by the caller.
    pub async fn sync(&self, store: &Mutex<Store>) -> Result<Outcome> {
        // Pull first; persist each applied page and its cursor together.
        let mut pages = 0;
        loop {
            let after = {
                let store = store.lock().await;
                store.check_binding(&self.binding)?;
                store.state.cursor
            };
            if pages == self.pages_per_pass {
                return Ok(Outcome::MoreHistory { cursor: after });
            }
            let response = self
                .client
                .get(self.url("changes")?)
                .query(&[("after", after)])
                .bearer_auth(&self.token)
                .send()
                .await
                .map_err(|_| "Sync connection failed. Local changes are safe.")?;
            let page: Page = checked(response)?
                .json()
                .await
                .map_err(|_| "Invalid change feed response.")?;
            let has_more = page.has_more;
            {
                let mut store = store.lock().await;
                self.live()?;
                store.apply_page(after, page, &self.binding)?;
            }
            pages += 1;
            if !has_more {
                break;
            }
        }
        let uploads = {
            let mut store = store.lock().await;
            self.live()?;
            store.uploads()?
        };
        for upload in uploads {
            let url = self.url(&format!("settings/{}", upload.id))?;
            let body =
                serde_json::json!({ "baseVersion": upload.base_version, "value": upload.task });
            let request = if upload.task.is_some() {
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
            let remote = if response.status() == StatusCode::CONFLICT {
                let conflict: Conflict = response
                    .json()
                    .await
                    .map_err(|_| "Invalid conflict response.")?;
                Err(conflict
                    .current
                    .ok_or("Server record is missing; local changes were retained.".to_string())?)
            } else {
                let setting: Setting = checked(response)?
                    .json()
                    .await
                    .map_err(|_| "Invalid write response.")?;
                if setting.setting.key != upload.id {
                    return Err("Server returned a different task key.".into());
                }
                Ok(setting.setting)
            };
            let mut store = store.lock().await;
            self.live()?;
            match remote {
                Ok(remote) => store.acknowledge(&upload, remote)?,
                Err(current) => store.reject(current)?,
            }
        }
        Ok(Outcome::Complete)
    }
}

impl Store {
    fn check_binding(&self, binding: &str) -> Result<()> {
        if self.state.binding.as_ref().is_some_and(|b| b != binding) {
            return Err("This local profile is bound to another token or server. Use the original credentials; account migration is not implemented yet.".into());
        }
        Ok(())
    }

    pub(crate) fn apply_page(&mut self, after: u64, page: Page, binding: &str) -> Result<()> {
        if self.state.cursor != after {
            return Err("The sync cursor changed during a request; retrying.".into());
        }
        if page.cursor < after || (page.has_more && page.cursor == after) {
            return Err("Invalid change feed cursor.".into());
        }
        let mut state = self.snapshot();
        for remote in page.changes {
            apply(&mut state, remote)?;
        }
        state.cursor = page.cursor;
        state.binding = Some(binding.into());
        self.commit(state)
    }

    /// Call only after the entire feed has been replayed.
    pub(crate) fn uploads(&mut self) -> Result<Vec<Upload>> {
        let mut state = self.snapshot();
        reconcile_identical(&mut state);
        // A locally created and deleted item has never existed remotely.
        state.records.retain(|_, r| {
            !(r.dirty && r.conflict.is_none() && r.version == 0 && r.task.is_none())
        });
        self.commit(state)?;
        Ok(self
            .state
            .records
            .iter()
            .filter(|(_, r)| r.dirty && r.conflict.is_none())
            .map(|(id, r)| Upload {
                id: id.clone(),
                task: r.task.clone(),
                base_version: r.version,
                local_revision: r.local_revision,
            })
            .collect())
    }

    pub(crate) fn acknowledge(&mut self, upload: &Upload, remote: Remote) -> Result<()> {
        let mut state = self.snapshot();
        let record = state
            .records
            .get_mut(&upload.id)
            .ok_or("Task no longer exists.")?;
        if record.local_revision != upload.local_revision && remote.value == upload.task {
            // The newer local edit was made on top of the value just stored, so keep it pending
            // with the acknowledged version as its base.
            record.version = record.version.max(remote.version);
        } else {
            // Unchanged records are acknowledged; an unexpected stored value becomes a conflict.
            apply(&mut state, remote)?;
            reconcile_identical(&mut state);
        }
        self.commit(state)
    }

    pub(crate) fn reject(&mut self, current: Remote) -> Result<()> {
        let mut state = self.snapshot();
        apply(&mut state, current)?;
        reconcile_identical(&mut state);
        self.commit(state)
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
    let record = state.records.entry(remote.key.clone()).or_default();
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
        let reminder = |task: Option<&Task>| task.map(|task| (task.remind_at, task.completed));
        if reminder(record.task.as_ref()) != reminder(remote.value.as_ref()) {
            record.snooze = None;
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
    fn edit_during_upload_stays_pending_on_the_acknowledged_base() {
        let mut store = Store::open(":memory:").unwrap();
        let id = store.save_task(None, task("Sent")).unwrap();
        let [upload] = store.uploads().unwrap().try_into().unwrap();
        // The lock is released during the request, so the user edits again.
        store.save_task(Some(id.clone()), task("Newer")).unwrap();
        store
            .acknowledge(&upload, remote(&id, Some("Sent"), 1))
            .unwrap();
        let record = &store.state.records[&id];
        assert!(record.dirty && record.conflict.is_none());
        assert_eq!(
            (record.version, record.task.as_ref().unwrap().title.as_str()),
            (1, "Newer")
        );
        let [next] = store.uploads().unwrap().try_into().unwrap();
        assert_eq!(
            (next.base_version, next.task.unwrap().title),
            (1, "Newer".into())
        );

        // Deleting during a request keeps the tombstone pending too.
        store.delete_task(&id).unwrap();
        let upload = Upload {
            id: id.clone(),
            task: Some(task("Newer")),
            base_version: 1,
            local_revision: 2,
        };
        store
            .acknowledge(&upload, remote(&id, Some("Newer"), 2))
            .unwrap();
        let record = &store.state.records[&id];
        assert!(record.dirty && record.task.is_none() && record.version == 2);
    }

    #[test]
    fn unchanged_upload_is_acknowledged_and_unexpected_value_conflicts() {
        let mut store = Store::open(":memory:").unwrap();
        let id = store.save_task(None, task("Sent")).unwrap();
        let [upload] = store.uploads().unwrap().try_into().unwrap();
        store
            .acknowledge(&upload, remote(&id, Some("Sent"), 1))
            .unwrap();
        assert!(!store.state.records[&id].dirty);
        assert!(store.uploads().unwrap().is_empty());

        store.save_task(Some(id.clone()), task("Mine")).unwrap();
        let [upload] = store.uploads().unwrap().try_into().unwrap();
        store
            .acknowledge(&upload, remote(&id, Some("Other"), 2))
            .unwrap();
        assert_eq!(
            store.state.records[&id].conflict.as_ref().unwrap().version,
            2
        );
        assert!(store.uploads().unwrap().is_empty());
    }

    #[test]
    fn rejected_write_records_the_server_value_as_a_conflict() {
        let mut store = Store::open(":memory:").unwrap();
        let id = store.save_task(None, task("Mine")).unwrap();
        store.reject(remote(&id, Some("Theirs"), 3)).unwrap();
        let record = &store.state.records[&id];
        assert_eq!(record.task.as_ref().unwrap().title, "Mine");
        assert_eq!(record.conflict.as_ref().unwrap().version, 3);
    }

    #[test]
    fn local_only_tombstones_are_dropped_and_stale_pages_are_refused() {
        let mut store = Store::open(":memory:").unwrap();
        let id = store.save_task(None, task("Short-lived")).unwrap();
        store.delete_task(&id).unwrap();
        assert!(store.uploads().unwrap().is_empty());
        assert!(store.state.records.is_empty());

        let page = |cursor| Page {
            changes: vec![],
            cursor,
            has_more: false,
        };
        store.apply_page(0, page(5), "b").unwrap();
        assert!(store.apply_page(0, page(9), "b").is_err());
        assert_eq!(store.state.cursor, 5);
        assert!(store.check_binding("other").is_err());
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

    #[test]
    fn snooze_stays_local_and_follows_the_synced_reminder() {
        let mut store = Store::open(":memory:").unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let timed = |title: &str, remind_at| Remote {
            value: Some(Task {
                remind_at: Some(remind_at),
                ..task(title)
            }),
            ..remote(&id, Some(title), 0)
        };
        apply(
            &mut store.state,
            Remote {
                version: 1,
                ..timed("Call", 1000)
            },
        )
        .unwrap();
        // Not yet due: snoozing would bring it forward.
        assert!(store.snooze(&id, 999, 1500).is_err());
        store.snooze(&id, 1000, 5000).unwrap();
        let record = &store.state.records[&id];
        assert!(!record.dirty && record.local_revision == 0);
        assert!(store.uploads().unwrap().is_empty());
        assert_eq!(store.due(4999).len(), 0);
        assert_eq!(store.due(5000)[0].2, 5000);
        // Another device renames the task: the snooze still applies to the same reminder.
        apply(
            &mut store.state,
            Remote {
                version: 2,
                ..timed("Call Sam", 1000)
            },
        )
        .unwrap();
        assert_eq!(store.state.records[&id].reminder_at(), Some(5000));
        // Another device moves the reminder: the old snooze no longer applies.
        apply(
            &mut store.state,
            Remote {
                version: 3,
                ..timed("Call Sam", 9000)
            },
        )
        .unwrap();
        assert_eq!(store.state.records[&id].snooze, None);
        assert_eq!(store.state.records[&id].reminder_at(), Some(9000));
        // Builds before this rule could save a snooze earlier than the reminder; it is ignored.
        let record = store.state.records.get_mut(&id).unwrap();
        record.snooze = Some(crate::Snooze {
            of: 9000,
            until: 2000,
        });
        assert_eq!(record.reminder_at(), Some(9000));
        assert!(store.due(2000).is_empty());
    }
}
