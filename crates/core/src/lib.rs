//! Platform-independent task storage and Astraea Sync protocol.
pub mod schedule;
pub mod sync;

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Task {
    pub schema_version: u32,
    pub title: String,
    pub notes: String,
    pub completed: bool,
    /// UTC Unix milliseconds. None means no reminder.
    pub remind_at: Option<i64>,
}

impl Task {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            return Err("Unsupported task schema; update the app before syncing.".into());
        }
        if self.title.trim().is_empty() || self.title.len() > 500 || self.notes.len() > 8000 {
            return Err("Use a title of 1–500 bytes and notes up to 8000 bytes.".into());
        }
        if self
            .remind_at
            .is_some_and(|time| !(0..=8_640_000_000_000_000).contains(&time))
        {
            return Err("Reminder date is out of range.".into());
        }
        let body = serde_json::json!({"baseVersion": 9_007_199_254_740_990_u64, "value": self});
        if serde_json::to_vec(&body).map_err(|e| e.to_string())?.len() > 16 * 1024 {
            return Err("Task exceeds the sync API's 16 KiB limit.".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Remote {
    pub key: String,
    pub value: Option<Task>,
    pub version: u64,
    pub deleted: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub task: Option<Task>,
    pub version: u64,
    pub dirty: bool,
    pub conflict: Option<Remote>,
    /// Local delivery receipt, deliberately never synced between devices.
    pub notified_at: Option<i64>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub records: BTreeMap<String, Record>,
    pub cursor: u64,
    pub binding: Option<String>,
}

pub struct Store {
    db: Connection,
    pub(crate) state: Snapshot,
    persisted: Option<String>,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db = Connection::open(path).map_err(|e| e.to_string())?;
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS app_state (id INTEGER PRIMARY KEY CHECK(id=1), json TEXT NOT NULL);")
            .map_err(|e| e.to_string())?;
        let json: Option<String> = db
            .query_row("SELECT json FROM app_state WHERE id=1", [], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|e| e.to_string())?;
        let state = match &json {
            Some(json) => {
                serde_json::from_str(json).map_err(|e| format!("Cannot read saved data: {e}"))?
            }
            None => Snapshot::default(),
        };
        Ok(Self {
            db,
            state,
            persisted: json,
        })
    }

    pub fn snapshot(&self) -> Snapshot {
        self.state.clone()
    }

    /// Commit first, then expose the new state. Failed disk writes never acknowledge edits.
    pub(crate) fn commit(&mut self, state: Snapshot) -> Result<()> {
        let json = serde_json::to_string(&state).map_err(|e| e.to_string())?;
        let changed = match &self.persisted {
            Some(previous) => self.db.execute(
                "UPDATE app_state SET json=?1 WHERE id=1 AND json=?2",
                params![json, previous],
            ),
            None => self.db.execute(
                "INSERT INTO app_state(id,json) VALUES(1,?1) ON CONFLICT(id) DO NOTHING",
                params![json],
            ),
        }
        .map_err(|e| e.to_string())?;
        if changed != 1 {
            return Err("Another app instance changed this profile. Close other instances and restart before editing or syncing.".into());
        }
        self.persisted = Some(json);
        self.state = state;
        Ok(())
    }

    pub fn save_task(&mut self, id: Option<String>, mut task: Task) -> Result<String> {
        task.title = task.title.trim().into();
        task.validate()?;
        let mut state = self.snapshot();
        let id = id.unwrap_or_else(|| Uuid::new_v4().to_string());
        if Uuid::parse_str(&id).is_err() {
            return Err("Invalid task ID.".into());
        }
        let record = state.records.entry(id.clone()).or_insert(Record {
            task: None,
            version: 0,
            dirty: false,
            conflict: None,
            notified_at: None,
        });
        if record.task.as_ref().map(|t| (t.remind_at, t.completed))
            != Some((task.remind_at, task.completed))
        {
            record.notified_at = None;
        }
        record.task = Some(task);
        record.dirty = true;
        self.commit(state)?;
        Ok(id)
    }

    pub fn delete_task(&mut self, id: &str) -> Result<()> {
        let mut state = self.snapshot();
        let record = state.records.get_mut(id).ok_or("Task no longer exists.")?;
        record.task = None;
        record.dirty = true;
        record.notified_at = None;
        self.commit(state)
    }

    pub fn resolve(&mut self, id: &str, keep_local: bool) -> Result<()> {
        let mut state = self.snapshot();
        let record = state.records.get_mut(id).ok_or("Task no longer exists.")?;
        let remote = record.conflict.take().ok_or("No conflict to resolve.")?;
        record.version = remote.version;
        if keep_local {
            record.dirty = record.task != remote.value;
        } else {
            record.task = remote.value;
            record.dirty = false;
            record.notified_at = None;
        }
        self.commit(state)
    }

    pub fn due(&self, now: i64) -> Vec<(String, Task)> {
        self.state
            .records
            .iter()
            .filter_map(|(id, record)| {
                let task = record.task.as_ref()?;
                let due = task.remind_at?;
                (!task.completed && due <= now && record.notified_at != Some(due))
                    .then(|| (id.clone(), task.clone()))
            })
            .collect()
    }

    pub fn mark_notified(&mut self, id: &str, due: i64) -> Result<()> {
        let mut state = self.snapshot();
        state
            .records
            .get_mut(id)
            .ok_or("Task no longer exists.")?
            .notified_at = Some(due);
        self.commit(state)
    }
}

use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;
    fn task() -> Task {
        Task {
            schema_version: 1,
            title: "Test".into(),
            notes: "".into(),
            completed: false,
            remind_at: Some(1000),
        }
    }

    #[test]
    fn persists_tasks_deletions_and_delivery_receipts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.sqlite3");
        let mut store = Store::open(&path).unwrap();
        let id = store.save_task(None, task()).unwrap();
        assert!(store.due(999).is_empty());
        assert_eq!(store.due(1000).len(), 1);
        store.mark_notified(&id, 1000).unwrap();
        drop(store);
        let mut store = Store::open(&path).unwrap();
        assert!(store.due(2000).is_empty());
        let mut snoozed = task();
        snoozed.remind_at = Some(3000);
        store.save_task(Some(id.clone()), snoozed).unwrap();
        assert_eq!(store.due(3000).len(), 1);
        store.delete_task(&id).unwrap();
        assert!(store.snapshot().records[&id].task.is_none());
        assert!(store.due(4000).is_empty());
    }

    #[test]
    fn stale_process_cannot_overwrite_saved_edits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.sqlite3");
        let mut a = Store::open(&path).unwrap();
        let mut b = Store::open(&path).unwrap();
        let id = a.save_task(None, task()).unwrap();
        assert!(b.save_task(None, task()).is_err());
        assert!(b.snapshot().records.is_empty());
        assert!(
            Store::open(&path)
                .unwrap()
                .snapshot()
                .records
                .contains_key(&id)
        );
    }

    #[test]
    fn validates_payload_before_storage() {
        let mut store = Store::open(":memory:").unwrap();
        let mut t = task();
        t.title = "  ".into();
        assert!(store.save_task(None, t).is_err());
        let mut t = task();
        t.notes = "\u{0001}".repeat(8000);
        assert!(store.save_task(None, t).is_err());
        assert!(store.snapshot().records.is_empty());
    }
}
