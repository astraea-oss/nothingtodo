use crate::identity::{self, MARKER_KEY, Next};
use crate::{Opaque, Remote, Result, SCHEMA_VERSION, Snapshot, Store, Task};
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

/// A record as the server sends it, before deciding whether this version understands it.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Raw {
    key: String,
    value: Option<serde_json::Value>,
    version: u64,
    deleted: bool,
}

impl From<Remote> for Raw {
    fn from(remote: Remote) -> Self {
        Raw {
            key: remote.key,
            value: remote
                .value
                .map(|task| serde_json::to_value(task).expect("tasks serialize")),
            version: remote.version,
            deleted: remote.deleted,
        }
    }
}

/// A task this version fully supports, or None for a newer schema or an invalid payload.
fn supported(value: &serde_json::Value) -> Option<Task> {
    let task: Task = serde_json::from_value(value.clone()).ok()?;
    (task.schema_version == SCHEMA_VERSION && task.validate().is_ok()).then_some(task)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Page {
    changes: Vec<Raw>,
    cursor: u64,
    has_more: bool,
}
#[derive(Deserialize)]
struct Setting {
    setting: Raw,
}
#[derive(Deserialize)]
struct Conflict {
    current: Option<Raw>,
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

    /// The account ID in this connection's identity marker, or None if there is none.
    async fn fetch_marker(&self) -> Result<Option<String>> {
        let response = self
            .client
            .get(self.url(&format!("settings/{MARKER_KEY}"))?)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| "Sync connection failed. Local changes are safe.")?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let setting: Setting = checked(response)?
            .json()
            .await
            .map_err(|_| "Invalid identity response.")?;
        Ok(identity::marker_id(Some(&Opaque {
            value: setting.setting.value,
            version: setting.setting.version,
        })))
    }

    /// A different token or server for an existing profile: accept it only if it reaches the
    /// account the profile recorded, before applying anything from it.
    async fn verify_new_binding(&self, store: &Mutex<Store>) -> Result<()> {
        let changed = {
            let store = store.lock().await;
            self.live()?;
            store
                .state
                .binding
                .as_ref()
                .is_some_and(|b| b != &self.binding)
        };
        if !changed {
            return Ok(());
        }
        let marker = self.fetch_marker().await?;
        let mut store = store.lock().await;
        self.live()?;
        store.rebind(&self.binding, marker)
    }

    /// After a full replay with verified credentials: confirm the account, creating its marker if
    /// no device has yet. Concurrent creators race on `baseVersion: 0`; the loser adopts the
    /// winner's marker.
    async fn confirm_account(&self, store: &Mutex<Store>) -> Result<()> {
        let next = {
            let mut store = store.lock().await;
            self.live()?;
            store.check_account()?
        };
        if let Next::Ready = next {
            return Ok(());
        }
        let id = uuid::Uuid::new_v4().to_string();
        let body = serde_json::json!({ "baseVersion": 0, "value": identity::marker_value(&id) });
        let response = self
            .client
            .put(self.url(&format!("settings/{MARKER_KEY}"))?)
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .map_err(|_| "Sync connection failed. Local changes are safe.")?;
        let marker = if response.status() == StatusCode::CONFLICT {
            let conflict: Conflict = response
                .json()
                .await
                .map_err(|_| "Invalid conflict response.")?;
            conflict
                .current
                .ok_or("Identity marker missing after a conflict.")?
        } else {
            let setting: Setting = checked(response)?
                .json()
                .await
                .map_err(|_| "Invalid write response.")?;
            setting.setting
        };
        let mut store = store.lock().await;
        self.live()?;
        let mut state = store.snapshot();
        apply(&mut state, marker)?;
        store.commit(state)?;
        store.check_account().map(|_| ())
    }

    /// The store lock is held only to read or commit state, never across a network request.
    /// Network attempts must still be serialized by the caller.
    pub async fn sync(&self, store: &Mutex<Store>) -> Result<Outcome> {
        self.verify_new_binding(store).await?;
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
        self.confirm_account(store).await?;
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
            return Err("This local profile is bound to another token or server, and that token's account could not be confirmed. Nothing was downloaded or uploaded.".into());
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
            // Uploading over a newer schema would destroy data this version cannot read.
            .filter(|(_, r)| r.dirty && r.conflict.is_none() && r.editable())
            .map(|(id, r)| Upload {
                id: id.clone(),
                task: r.task.clone(),
                base_version: r.version,
                local_revision: r.local_revision,
            })
            .collect())
    }

    pub(crate) fn acknowledge(&mut self, upload: &Upload, remote: impl Into<Raw>) -> Result<()> {
        let remote = remote.into();
        let mut state = self.snapshot();
        let record = state
            .records
            .get_mut(&upload.id)
            .ok_or("Task no longer exists.")?;
        let stored = remote.value.as_ref().map(supported);
        if record.local_revision != upload.local_revision && stored == upload.task.clone().map(Some)
        {
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

    pub(crate) fn reject(&mut self, current: impl Into<Raw>) -> Result<()> {
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

fn apply(state: &mut Snapshot, remote: impl Into<Raw>) -> Result<()> {
    let raw = remote.into();
    if raw.version == 0 || raw.deleted != raw.value.is_none() {
        return Err("Invalid remote record.".into());
    }
    // Only UUID keys are tasks. Anything else belongs to another record type, such as one added
    // by a newer version; keep it so it is never lost, and carry on syncing tasks.
    if uuid::Uuid::parse_str(&raw.key).is_err() {
        let kept = state.foreign.entry(raw.key).or_insert(Opaque {
            value: None,
            version: 0,
        });
        if raw.version > kept.version {
            *kept = Opaque {
                value: raw.value,
                version: raw.version,
            };
        }
        return Ok(());
    }
    let value = match raw.value {
        None => None,
        Some(value) => match supported(&value) {
            Some(task) => Some(task),
            None => {
                let opaque = Opaque {
                    value: Some(value),
                    version: raw.version,
                };
                apply_unsupported(state, raw.key, opaque);
                return Ok(());
            }
        },
    };
    let remote = Remote {
        key: raw.key,
        value,
        version: raw.version,
        deleted: raw.deleted,
    };
    let record = state.records.entry(remote.key.clone()).or_default();
    if remote.version <= record.seen() {
        return Ok(());
    }
    // A supported value newer than an unsupported one makes the record editable again.
    record.unsupported = None;
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

/// A newer schema arrived for a task. Display keeps the last value this version understood;
/// see `Record::unsupported` for why a pending edit keeps its base version.
fn apply_unsupported(state: &mut Snapshot, key: String, opaque: Opaque) {
    let record = state.records.entry(key).or_default();
    if opaque.version <= record.seen() {
        return;
    }
    if !record.dirty {
        record.version = opaque.version;
        record.conflict = None;
    }
    record.notified_at = None;
    record.snooze = None;
    record.unsupported = Some(opaque);
}

// Compare against the latest event only, after the entire feed has been replayed.
// An older event matching a pending edit must never acknowledge that edit.
fn reconcile_identical(state: &mut Snapshot) {
    for record in state.records.values_mut().filter(|r| r.editable()) {
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
pub(crate) mod tests_support {
    use crate::{Remote, Store};

    pub fn apply_remote(store: &mut Store, remote: Remote) {
        super::apply(&mut store.state, remote).unwrap();
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
            extra: Default::default(),
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
        let mut store = Store::open(":memory:").unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let page = Page {
            changes: vec![
                remote(&id, Some("Fine"), 1).into(),
                raw(&uuid::Uuid::new_v4().to_string(), None, 0),
            ],
            cursor: 2,
            has_more: false,
        };
        assert!(store.apply_page(0, page, "binding").is_err());
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

    fn raw(key: &str, value: Option<serde_json::Value>, version: u64) -> Raw {
        Raw {
            key: key.into(),
            deleted: value.is_none(),
            value,
            version,
        }
    }

    #[test]
    fn additive_fields_survive_sync_and_local_edits() {
        let mut store = Store::open(":memory:").unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let value = serde_json::json!({
            "schemaVersion": 1, "title": "Call", "notes": "", "completed": false,
            "remindAt": null, "colour": "teal"
        });
        apply(&mut store.state, raw(&id, Some(value), 1)).unwrap();
        // An editor that only knows this version's fields saves the task.
        store.save_task(Some(id.clone()), task("Call Sam")).unwrap();
        let [upload] = store.uploads().unwrap().try_into().unwrap();
        let sent = serde_json::to_value(upload.task.unwrap()).unwrap();
        assert_eq!(sent["colour"], "teal");
        assert_eq!(sent["title"], "Call Sam");
    }

    #[test]
    fn newer_schema_is_read_only_and_never_uploaded_or_scheduled() {
        let mut store = Store::open(":memory:").unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let mut timed = task("Call");
        timed.remind_at = Some(5000);
        apply(
            &mut store.state,
            Remote {
                key: id.clone(),
                value: Some(timed),
                version: 1,
                deleted: false,
            },
        )
        .unwrap();
        let newer = serde_json::json!({ "schemaVersion": 2, "title": "Call", "kind": "alarm" });
        apply(&mut store.state, raw(&id, Some(newer.clone()), 2)).unwrap();
        let record = &store.state.records[&id];
        assert!(!record.editable() && record.version == 2);
        assert_eq!(
            record.task.as_ref().unwrap().title,
            "Call",
            "last known value stays for display"
        );
        assert!(store.save_task(Some(id.clone()), task("Edit")).is_err());
        assert!(store.delete_task(&id).is_err());
        assert!(store.alarms(0).unwrap().is_empty());
        assert!(store.due(10_000).is_empty());
        assert!(store.uploads().unwrap().is_empty());
        // Replaying an older supported event does not undo it; a newer supported one does.
        apply(
            &mut store.state,
            Remote {
                key: id.clone(),
                value: Some(task("Old")),
                version: 1,
                deleted: false,
            },
        )
        .unwrap();
        assert!(!store.state.records[&id].editable());
        apply(
            &mut store.state,
            Remote {
                key: id.clone(),
                value: Some(task("Back")),
                version: 3,
                deleted: false,
            },
        )
        .unwrap();
        assert!(store.state.records[&id].editable());
    }

    #[test]
    fn pending_edit_is_held_back_when_a_newer_schema_arrives() {
        let mut store = Store::open(":memory:").unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        apply(
            &mut store.state,
            Remote {
                key: id.clone(),
                value: Some(task("Call")),
                version: 1,
                deleted: false,
            },
        )
        .unwrap();
        store
            .save_task(Some(id.clone()), task("Local edit"))
            .unwrap();
        apply(
            &mut store.state,
            raw(&id, Some(serde_json::json!({ "schemaVersion": 2 })), 2),
        )
        .unwrap();
        let record = &store.state.records[&id];
        // Still dirty on its old base, so a newer client sees a conflict rather than a lost edit.
        assert!(record.dirty && record.version == 1 && !record.editable());
        assert!(store.uploads().unwrap().is_empty());
    }

    #[test]
    fn foreign_records_are_kept_and_do_not_stop_sync() {
        let mut store = Store::open(":memory:").unwrap();
        let marker = serde_json::json!({ "type": "account", "id": "abc" });
        let page = Page {
            changes: vec![
                raw("account-marker", Some(marker.clone()), 1),
                Remote {
                    key: uuid::Uuid::new_v4().to_string(),
                    value: Some(task("Still synced")),
                    version: 2,
                    deleted: false,
                }
                .into(),
            ],
            cursor: 2,
            has_more: false,
        };
        store.apply_page(0, page, "binding").unwrap();
        assert_eq!(store.state.cursor, 2);
        assert_eq!(store.state.foreign["account-marker"].value, Some(marker));
        assert_eq!(store.state.records.len(), 1);
        assert!(store.uploads().unwrap().is_empty());
    }

    #[test]
    fn invalid_or_malformed_task_payloads_become_read_only_instead_of_stalling() {
        let mut store = Store::open(":memory:").unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        apply(
            &mut store.state,
            raw(&id, Some(serde_json::json!("not an object")), 1),
        )
        .unwrap();
        assert!(!store.state.records[&id].editable());
        assert!(store.state.records[&id].task.is_none());
    }
}
