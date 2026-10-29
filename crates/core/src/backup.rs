//! Versioned backups. Credentials live in the system keyring and are never part of a snapshot,
//! so they are never exported. Exports are plaintext and must be kept private.
use crate::{Record, Result, SCHEMA_VERSION, Snapshot, Store};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const FORMAT: &str = "nothingtodo-backup";
/// Bump when the file layout changes; older apps refuse newer files rather than drop data.
pub const FORMAT_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Backup {
    pub format: String,
    pub format_version: u32,
    /// Unix milliseconds.
    pub exported_at: i64,
    pub snapshot: Snapshot,
}

/// What a restore or copy did, for the UI to report.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub tasks: usize,
    /// Records this version cannot interpret. A restore keeps them; a copy leaves them out.
    pub skipped: usize,
    pub exported_at: i64,
}

/// Parses and checks a backup file without changing anything.
pub fn parse(json: &str) -> Result<Backup> {
    let backup: Backup =
        serde_json::from_str(json).map_err(|_| "This is not a NothingToDo backup file.")?;
    if backup.format != FORMAT {
        return Err("This is not a NothingToDo backup file.".into());
    }
    if backup.format_version > FORMAT_VERSION {
        return Err(
            "This backup was made by a newer version of NothingToDo. Update the app to restore it."
                .into(),
        );
    }
    Ok(backup)
}

fn live_tasks(snapshot: &Snapshot) -> usize {
    snapshot
        .records
        .values()
        .filter(|r| r.task.is_some() && r.editable())
        .count()
}

fn unsupported(snapshot: &Snapshot) -> usize {
    snapshot.records.values().filter(|r| !r.editable()).count() + snapshot.foreign.len()
}

/// Delivery receipts, snoozes and notification IDs belong to one device and one moment. A restore
/// starts them afresh: reminders already past show as overdue, future ones are scheduled again.
fn reset_device_state(snapshot: &mut Snapshot) {
    snapshot.notification_ids = Default::default();
    for record in snapshot.records.values_mut() {
        record.notified_at = None;
        record.snooze = None;
    }
}

impl Store {
    pub fn export(&self, now: i64) -> Result<String> {
        let mut snapshot = self.snapshot();
        reset_device_state(&mut snapshot);
        serde_json::to_string_pretty(&Backup {
            format: FORMAT.into(),
            format_version: FORMAT_VERSION,
            exported_at: now,
            snapshot,
        })
        .map_err(|e| e.to_string())
    }

    /// Replaces this profile with the backup, as a coherent snapshot and cursor. Sync then
    /// replays everything since the backup before uploading, so remote edits and deletions made
    /// meanwhile surface as conflicts instead of being overwritten. The backup's account binding
    /// is kept as a claim: uploads are refused unless the connected account matches it.
    pub fn restore(&mut self, backup: Backup) -> Result<Summary> {
        let mut snapshot = backup.snapshot;
        reset_device_state(&mut snapshot);
        let summary = Summary {
            tasks: live_tasks(&snapshot),
            skipped: unsupported(&snapshot),
            exported_at: backup.exported_at,
        };
        // Revisions only need to differ from any upload captured before the restore.
        let base = self
            .state
            .records
            .values()
            .map(|r| r.local_revision)
            .max()
            .unwrap_or(0);
        for record in snapshot.records.values_mut() {
            record.local_revision += base + 1;
        }
        self.commit(snapshot)?;
        Ok(summary)
    }

    /// Adds the backup's tasks as new tasks with fresh IDs and no sync history, for moving tasks
    /// into another profile or account. Records this version cannot interpret are left out.
    pub fn copy_from(&mut self, backup: Backup) -> Result<Summary> {
        let mut state = self.snapshot();
        let mut summary = Summary {
            skipped: unsupported(&backup.snapshot),
            exported_at: backup.exported_at,
            ..Default::default()
        };
        for record in backup.snapshot.records.into_values() {
            let Some(mut task) = record.task.filter(|_| record.unsupported.is_none()) else {
                continue;
            };
            task.schema_version = SCHEMA_VERSION;
            task.validate()?;
            state.records.insert(
                Uuid::new_v4().to_string(),
                Record {
                    task: Some(task),
                    dirty: true,
                    local_revision: 1,
                    ..Default::default()
                },
            );
            summary.tasks += 1;
        }
        self.commit(state)?;
        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Remote, Task};

    fn task(title: &str) -> Task {
        Task {
            schema_version: 1,
            title: title.into(),
            notes: "".into(),
            completed: false,
            remind_at: Some(1000),
            extra: Default::default(),
        }
    }

    #[test]
    fn export_round_trips_and_resets_device_state() {
        let mut store = Store::open(":memory:").unwrap();
        let id = store.save_task(None, task("Call")).unwrap();
        store.mark_notified(&id, 1000).unwrap();
        store.alarms(0).unwrap();
        let json = store.export(42).unwrap();
        assert!(!json.contains("token"));

        let mut other = Store::open(":memory:").unwrap();
        other.save_task(None, task("Replaced")).unwrap();
        let summary = other.restore(parse(&json).unwrap()).unwrap();
        assert_eq!(
            summary,
            Summary {
                tasks: 1,
                skipped: 0,
                exported_at: 42
            }
        );
        let record = &other.snapshot().records[&id];
        assert_eq!(record.task.as_ref().unwrap().title, "Call");
        assert!(record.dirty && record.notified_at.is_none());
        assert!(other.snapshot().notification_ids.assigned.is_empty());
        assert_eq!(other.snapshot().records.len(), 1);
    }

    #[test]
    fn restore_keeps_cursor_and_binding_together_and_replays_remote_deletions() {
        let mut store = Store::open(":memory:").unwrap();
        let id = Uuid::new_v4().to_string();
        let mut snapshot = store.snapshot();
        snapshot.cursor = 7;
        snapshot.binding = Some("account".into());
        snapshot.records.insert(
            id.clone(),
            Record {
                task: Some(task("Edited before backup")),
                version: 3,
                dirty: true,
                ..Default::default()
            },
        );
        store.commit(snapshot).unwrap();
        let json = store.export(1).unwrap();

        let mut restored = Store::open(":memory:").unwrap();
        restored.restore(parse(&json).unwrap()).unwrap();
        let state = restored.snapshot();
        assert_eq!(
            (state.cursor, state.binding.as_deref()),
            (7, Some("account"))
        );
        // Deleted elsewhere after the backup: the next replay turns the pending edit into a
        // conflict instead of resurrecting it.
        crate::sync::tests_support::apply_remote(
            &mut restored,
            Remote {
                key: id.clone(),
                value: None,
                version: 4,
                deleted: true,
            },
        );
        let record = &restored.snapshot().records[&id];
        assert!(record.conflict.as_ref().is_some_and(|c| c.deleted));
        assert_eq!(record.task.as_ref().unwrap().title, "Edited before backup");
    }

    #[test]
    fn copy_creates_fresh_unsynced_tasks_and_leaves_out_unsupported_records() {
        let mut source = Store::open(":memory:").unwrap();
        let id = source.save_task(None, task("Keep")).unwrap();
        let mut snapshot = source.snapshot();
        snapshot.cursor = 9;
        snapshot.binding = Some("other account".into());
        snapshot.records.get_mut(&id).unwrap().version = 5;
        snapshot.records.insert(
            Uuid::new_v4().to_string(),
            Record {
                task: Some(task("Newer")),
                unsupported: Some(crate::Opaque {
                    value: None,
                    version: 2,
                }),
                ..Default::default()
            },
        );
        source.commit(snapshot).unwrap();
        let json = source.export(5).unwrap();

        let mut target = Store::open(":memory:").unwrap();
        let summary = target.copy_from(parse(&json).unwrap()).unwrap();
        assert_eq!((summary.tasks, summary.skipped), (1, 1));
        let state = target.snapshot();
        assert_eq!((state.cursor, state.binding.as_deref()), (0, None));
        let (new_id, record) = state.records.iter().next().unwrap();
        assert_ne!(new_id, &id);
        assert!(record.dirty && record.version == 0);
    }

    #[test]
    fn refuses_other_files_and_newer_formats() {
        assert!(parse("{}").is_err());
        assert!(parse(r#"{"format":"x","formatVersion":1,"exportedAt":0,"snapshot":{"records":{},"cursor":0,"binding":null}}"#).is_err());
        let newer = r#"{"format":"nothingtodo-backup","formatVersion":99,"exportedAt":0,"snapshot":{"records":{},"cursor":0,"binding":null}}"#;
        assert!(parse(newer).err().unwrap().contains("newer version"));
    }
}
