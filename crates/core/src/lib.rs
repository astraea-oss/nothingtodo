//! Platform-independent task storage and Astraea Sync protocol.
pub mod alarms;
pub mod migrations;
pub mod schedule;
pub mod sync;

use rusqlite::{Connection, TransactionBehavior, params};
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

/// A device-local snooze of the reminder that was due at `of`. Never synced, and ignored once the
/// task's reminder time changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snooze {
    pub of: i64,
    pub until: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub task: Option<Task>,
    pub version: u64,
    pub dirty: bool,
    pub conflict: Option<Remote>,
    /// Local delivery receipt, deliberately never synced between devices.
    pub notified_at: Option<i64>,
    /// Incremented by every local edit, so sync can detect edits made during a request.
    pub local_revision: u64,
    /// Absent in profiles written before snoozing became device-local.
    #[serde(default)]
    pub snooze: Option<Snooze>,
}

impl Record {
    /// When this device should next notify: the synced reminder time, or a later local snooze.
    /// A snooze can only delay a reminder, never bring it forward.
    pub fn reminder_at(&self) -> Option<i64> {
        let remind_at = self.task.as_ref()?.remind_at?;
        Some(match self.snooze {
            Some(snooze) if snooze.of == remind_at && snooze.until > remind_at => snooze.until,
            _ => remind_at,
        })
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub records: BTreeMap<String, Record>,
    pub cursor: u64,
    pub binding: Option<String>,
    /// Device-local and never synced. Absent in profiles written before OS scheduling.
    #[serde(default)]
    pub notification_ids: alarms::NotificationIds,
}

pub struct Store {
    db: Connection,
    pub(crate) state: Snapshot,
    persisted: String,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut db = Connection::open(path).map_err(|e| e.to_string())?;
        migrations::migrate(&mut db)?;
        let json: String = db
            .query_row("SELECT json FROM snapshot WHERE id=1", [], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        let state =
            serde_json::from_str(&json).map_err(|e| format!("Cannot read saved data: {e}"))?;
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
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        // A newer binary may have migrated the profile since this process opened it.
        migrations::ensure_current(&tx)?;
        let changed = tx
            .execute(
                "UPDATE snapshot SET json=?1 WHERE id=1 AND json=?2",
                params![json, self.persisted],
            )
            .map_err(|e| e.to_string())?;
        if changed != 1 {
            return Err("Another app instance changed this profile. Close other instances and restart before editing or syncing.".into());
        }
        tx.commit().map_err(|e| e.to_string())?;
        self.persisted = json;
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
        let record = state.records.entry(id.clone()).or_default();
        if record.task.as_ref().map(|t| (t.remind_at, t.completed))
            != Some((task.remind_at, task.completed))
        {
            record.notified_at = None;
            record.snooze = None;
        }
        record.task = Some(task);
        record.dirty = true;
        record.local_revision += 1;
        self.commit(state)?;
        Ok(id)
    }

    pub fn delete_task(&mut self, id: &str) -> Result<()> {
        let mut state = self.snapshot();
        let record = state.records.get_mut(id).ok_or("Task no longer exists.")?;
        record.task = None;
        record.dirty = true;
        record.notified_at = None;
        record.snooze = None;
        record.local_revision += 1;
        self.commit(state)
    }

    pub fn resolve(&mut self, id: &str, keep_local: bool) -> Result<()> {
        let mut state = self.snapshot();
        let record = state.records.get_mut(id).ok_or("Task no longer exists.")?;
        let remote = record.conflict.take().ok_or("No conflict to resolve.")?;
        record.version = remote.version;
        record.local_revision += 1;
        if keep_local {
            record.dirty = record.task != remote.value;
        } else {
            record.task = remote.value;
            record.dirty = false;
            record.notified_at = None;
            record.snooze = None;
        }
        self.commit(state)
    }

    /// Reminders due on this device, with the instant each is due (a snooze moves it).
    pub fn due(&self, now: i64) -> Vec<(String, Task, i64)> {
        self.state
            .records
            .iter()
            .filter_map(|(id, record)| {
                let task = record.task.as_ref()?;
                let due = record.reminder_at()?;
                (!task.completed && due <= now && record.notified_at != Some(due))
                    .then(|| (id.clone(), task.clone(), due))
            })
            .collect()
    }

    /// Snoozes a reminder that has gone off, on this device only. Nothing is uploaded, so other
    /// devices keep the original time.
    pub fn snooze(&mut self, id: &str, now: i64, until: i64) -> Result<()> {
        let mut state = self.snapshot();
        let record = state.records.get_mut(id).ok_or("Task no longer exists.")?;
        let of = record
            .task
            .as_ref()
            .filter(|task| !task.completed)
            .and_then(|task| task.remind_at)
            .ok_or("Only active tasks with a reminder can be snoozed.")?;
        if record.reminder_at().is_none_or(|due| due > now) || until <= now {
            return Err("Only a reminder that has gone off can be snoozed.".into());
        }
        record.snooze = Some(Snooze { of, until });
        self.commit(state)
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
        store.snooze(&id, 2000, 3000).unwrap();
        drop(store);
        let mut store = Store::open(&path).unwrap();
        assert!(store.due(2999).is_empty());
        assert_eq!(store.due(3000)[0].2, 3000);
        store.mark_notified(&id, 3000).unwrap();
        assert!(store.due(4000).is_empty());
        // Editing the reminder time drops the snooze.
        let mut moved = task();
        moved.remind_at = Some(2000);
        store.save_task(Some(id.clone()), moved).unwrap();
        assert_eq!(store.snapshot().records[&id].snooze, None);
        assert_eq!(store.due(2000).len(), 1);
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
