//! Numbered SQLite migrations. `PRAGMA user_version` records the applied database version.
//!
//! Each migration transforms the stored JSON itself rather than decoding it with today's types,
//! so old migrations keep working after the in-memory model changes. All pending migrations and a
//! final decode of the result run in one transaction: a failure leaves the database untouched.
use crate::{Result, Snapshot};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

/// Migration `n` upgrades a database from version `n` to `n + 1`.
const MIGRATIONS: &[fn(&Transaction) -> Result<()>] = &[relocate_snapshot];

/// The only database version this binary reads or writes.
pub const DATABASE_VERSION: i64 = MIGRATIONS.len() as i64;

/// Replaces the legacy snapshot so binaries without migration support cannot decode it.
/// Their `Snapshot` decoder requires a `records` field, which this object deliberately lacks.
pub(crate) const LEGACY_SENTINEL: &str = r#"{"upgradeRequired":"This profile was upgraded by a newer NothingToDo. Install the current app; this version cannot read or change it.","databaseVersion":1}"#;

fn version(db: &Connection) -> Result<i64> {
    db.query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|e| e.to_string())
}

fn newer(found: i64) -> String {
    format!(
        "This profile uses database version {found}, newer than this app supports ({DATABASE_VERSION}). Update NothingToDo; the data was not modified."
    )
}

/// Rejects writes after another process has migrated the database past this binary's version.
pub(crate) fn ensure_current(db: &Connection) -> Result<()> {
    match version(db)? {
        DATABASE_VERSION => Ok(()),
        found => Err(format!(
            "Another NothingToDo version changed this profile to database version {found}. Close this instance and restart the current app."
        )),
    }
}

fn legacy_json(db: &Connection) -> Result<Option<String>> {
    let exists: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='app_state')",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    if !exists {
        return Ok(None);
    }
    db.query_row("SELECT json FROM app_state WHERE id=1", [], |row| {
        row.get(0)
    })
    .optional()
    .map_err(|e| e.to_string())
}

/// Brings the database to `DATABASE_VERSION`, refusing newer versions without modifying them.
pub(crate) fn migrate(db: &mut Connection) -> Result<()> {
    // Check before any statement that could write, including the journal mode change.
    let found = version(db)?;
    if found > DATABASE_VERSION {
        return Err(newer(found));
    }
    db.execute_batch("PRAGMA journal_mode=WAL;")
        .map_err(|e| e.to_string())?;
    if found == DATABASE_VERSION {
        return Ok(());
    }
    let legacy = if found == 0 { legacy_json(db)? } else { None };
    if legacy.is_some() {
        backup(db, found)?;
    }
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    // Another process may have migrated or written since the checks above.
    let current = version(&tx)?;
    if current > DATABASE_VERSION {
        return Err(newer(current));
    }
    if current == DATABASE_VERSION {
        return Ok(());
    }
    if current != found || (current == 0 && legacy_json(&tx)? != legacy) {
        return Err("The profile changed during the upgrade. Close other NothingToDo instances and restart.".into());
    }
    for migration in &MIGRATIONS[current as usize..] {
        migration(&tx)?;
    }
    let json: String = tx
        .query_row("SELECT json FROM snapshot WHERE id=1", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    serde_json::from_str::<Snapshot>(&json)
        .map_err(|e| format!("Cannot upgrade saved data: {e}. Nothing was changed."))?;
    tx.pragma_update(None, "user_version", DATABASE_VERSION)
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

/// Keeps an untouched copy beside the database. Never overwrites an earlier backup.
fn backup(db: &Connection, from: i64) -> Result<()> {
    let Some(path) = db.path().filter(|path| !path.is_empty()) else {
        return Ok(());
    };
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let target = format!("{path}.backup-v{from}-{millis}");
    db.execute("VACUUM INTO ?1", [&target])
        .map_err(|e| format!("Could not back up the profile before upgrading: {e}"))?;
    Ok(())
}

/// Version 1: move the snapshot to a new table, add per-record local revisions, and lock the
/// legacy table against binaries that predate migrations.
fn relocate_snapshot(tx: &Transaction) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS app_state (id INTEGER PRIMARY KEY CHECK(id=1), json TEXT NOT NULL);
         CREATE TABLE snapshot (id INTEGER PRIMARY KEY CHECK(id=1), json TEXT NOT NULL);",
    )
    .map_err(|e| e.to_string())?;
    let mut state: Value = match legacy_json(tx)? {
        Some(json) => serde_json::from_str(&json)
            .map_err(|e| format!("Cannot upgrade saved data: {e}. Nothing was changed."))?,
        None => json!({"records": {}, "cursor": 0, "binding": null}),
    };
    let records = state
        .get_mut("records")
        .and_then(Value::as_object_mut)
        .ok_or("Cannot upgrade saved data: records are missing. Nothing was changed.")?;
    for record in records.values_mut() {
        record
            .as_object_mut()
            .ok_or("Cannot upgrade saved data: invalid record. Nothing was changed.")?
            .insert("localRevision".into(), 0.into());
    }
    tx.execute(
        "INSERT INTO snapshot(id,json) VALUES(1,?1)",
        [state.to_string()],
    )
    .map_err(|e| e.to_string())?;
    // A sentinel row also stops an old binary from inserting a fresh snapshot into a new profile.
    tx.execute(
        "INSERT INTO app_state(id,json) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET json=excluded.json",
        [LEGACY_SENTINEL],
    )
    .map_err(|e| e.to_string())?;
    tx.execute_batch(
        "CREATE TRIGGER app_state_upgraded_insert BEFORE INSERT ON app_state
           BEGIN SELECT RAISE(ABORT, 'NothingToDo profile upgraded; install the current app'); END;
         CREATE TRIGGER app_state_upgraded_update BEFORE UPDATE ON app_state
           BEGIN SELECT RAISE(ABORT, 'NothingToDo profile upgraded; install the current app'); END;
         CREATE TRIGGER app_state_upgraded_delete BEFORE DELETE ON app_state
           BEGIN SELECT RAISE(ABORT, 'NothingToDo profile upgraded; install the current app'); END;",
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Store, Task};
    use rusqlite::params;
    use serde::Deserialize;
    use std::path::{Path, PathBuf};

    /// A frozen copy of the pre-migration binary's storage code and `Snapshot` shape.
    mod legacy {
        use super::*;
        #[allow(dead_code)]
        #[derive(Deserialize)]
        pub struct Snapshot {
            pub records: std::collections::BTreeMap<String, Value>,
            pub cursor: u64,
            pub binding: Option<String>,
        }
        pub struct Store {
            pub db: Connection,
            pub persisted: Option<String>,
        }
        pub fn open(path: &Path) -> std::result::Result<(Store, Snapshot), String> {
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
                Some(json) => serde_json::from_str(json)
                    .map_err(|e| format!("Cannot read saved data: {e}"))?,
                None => Snapshot {
                    records: Default::default(),
                    cursor: 0,
                    binding: None,
                },
            };
            Ok((
                Store {
                    db,
                    persisted: json,
                },
                state,
            ))
        }
        impl Store {
            pub fn commit(&mut self, json: &str) -> std::result::Result<(), String> {
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
                    return Err("Another app instance changed this profile.".into());
                }
                self.persisted = Some(json.into());
                Ok(())
            }
        }
    }

    const PENDING: &str = "00000000-0000-4000-8000-000000000001";
    const TOMBSTONE: &str = "00000000-0000-4000-8000-000000000002";
    const CONFLICTED: &str = "00000000-0000-4000-8000-000000000003";

    fn task(title: &str) -> Value {
        json!({"schemaVersion": 1, "title": title, "notes": "", "completed": false, "remindAt": 5000})
    }

    /// Data exactly as the pre-migration binary serialized it.
    fn legacy_state() -> String {
        json!({
            "records": {
                PENDING: {"task": task("Pending"), "version": 3, "dirty": true, "conflict": null, "notifiedAt": 5000},
                TOMBSTONE: {"task": null, "version": 7, "dirty": false, "conflict": null, "notifiedAt": null},
                CONFLICTED: {"task": task("Mine"), "version": 1, "dirty": true, "notifiedAt": null,
                    "conflict": {"key": CONFLICTED, "value": task("Theirs"), "version": 4, "deleted": false}},
            },
            "cursor": 42,
            "binding": "abc",
        })
        .to_string()
    }

    fn legacy_profile(dir: &Path) -> PathBuf {
        let path = dir.join("nothingtodo.sqlite3");
        let (mut old, _) = legacy::open(&path).unwrap();
        old.commit(&legacy_state()).unwrap();
        path
    }

    fn raw(path: &Path) -> Connection {
        Connection::open(path).unwrap()
    }

    fn stored_snapshot(path: &Path) -> String {
        raw(path)
            .query_row("SELECT json FROM snapshot WHERE id=1", [], |row| row.get(0))
            .unwrap()
    }

    fn backups(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.to_string_lossy().contains(".backup-v0-"))
            .collect()
    }

    #[test]
    fn legacy_snapshot_migrates_without_losing_sync_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = legacy_profile(dir.path());
        let store = Store::open(&path).unwrap();
        let state = store.snapshot();
        assert_eq!(version(&raw(&path)).unwrap(), DATABASE_VERSION);
        assert_eq!((state.cursor, state.binding.as_deref()), (42, Some("abc")));
        let pending = &state.records[PENDING];
        assert!(pending.dirty && pending.version == 3 && pending.notified_at == Some(5000));
        assert_eq!(pending.local_revision, 0);
        let tombstone = &state.records[TOMBSTONE];
        assert!(tombstone.task.is_none() && tombstone.version == 7 && !tombstone.dirty);
        let conflicted = &state.records[CONFLICTED];
        assert_eq!(conflicted.task.as_ref().unwrap().title, "Mine");
        assert_eq!(conflicted.conflict.as_ref().unwrap().version, 4);

        // The untouched pre-migration copy is still readable by the previous binary.
        let [backup] = backups(dir.path()).try_into().unwrap();
        let (_, old) = legacy::open(&backup).unwrap();
        assert_eq!((old.records.len(), old.cursor), (3, 42));
    }

    #[test]
    fn previous_binary_cannot_open_or_write_a_migrated_profile() {
        let dir = tempfile::tempdir().unwrap();
        let path = legacy_profile(dir.path());
        // Connected before the upgrade, as a still-running old instance would be.
        let (mut running, _) = legacy::open(&path).unwrap();
        let mut store = Store::open(&path).unwrap();
        store
            .save_task(
                Some(PENDING.into()),
                serde_json::from_value(task("New")).unwrap(),
            )
            .unwrap();
        let before = stored_snapshot(&path);

        assert!(running.commit(&legacy_state()).is_err());
        running.persisted = Some(LEGACY_SENTINEL.into());
        assert!(running.commit(&legacy_state()).is_err());
        running.persisted = None;
        assert!(running.commit(&legacy_state()).is_err());
        assert!(running.db.execute("DELETE FROM app_state", []).is_err());

        let error = legacy::open(&path).err().unwrap();
        assert!(error.contains("missing field `records`"), "{error}");

        assert_eq!(stored_snapshot(&path), before);
        assert_eq!(version(&raw(&path)).unwrap(), DATABASE_VERSION);
        let reopened = Store::open(&path).unwrap().snapshot();
        assert_eq!(reopened.records[PENDING].local_revision, 1);
        assert_eq!(reopened.cursor, 42);
    }

    #[test]
    fn new_profiles_are_guarded_against_the_previous_binary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nothingtodo.sqlite3");
        Store::open(&path).unwrap();
        assert!(backups(dir.path()).is_empty());
        assert!(legacy::open(&path).is_err());
        let db = raw(&path);
        assert!(
            db.execute("INSERT OR REPLACE INTO app_state VALUES(1,'{}')", [])
                .is_err()
        );
    }

    #[test]
    fn newer_database_versions_are_refused_without_modification() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nothingtodo.sqlite3");
        Store::open(&path).unwrap();
        raw(&path)
            .pragma_update(None, "user_version", DATABASE_VERSION + 1)
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let error = Store::open(&path).err().unwrap();
        assert!(error.contains("newer than this app supports"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn running_instance_cannot_write_after_a_newer_migration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nothingtodo.sqlite3");
        let mut store = Store::open(&path).unwrap();
        raw(&path)
            .pragma_update(None, "user_version", DATABASE_VERSION + 1)
            .unwrap();
        let before = stored_snapshot(&path);
        let task: Task = serde_json::from_value(task("Late")).unwrap();
        assert!(store.save_task(None, task).is_err());
        assert!(store.snapshot().records.is_empty());
        assert_eq!(stored_snapshot(&path), before);
    }

    #[test]
    fn failed_migration_rolls_back_completely() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nothingtodo.sqlite3");
        let (mut old, _) = legacy::open(&path).unwrap();
        // Readable JSON that cannot be decoded as a snapshot fails after tables were created.
        let unreadable = r#"{"records":{"x":{"task":"oops"}},"cursor":0,"binding":null}"#;
        old.commit(unreadable).unwrap();
        drop(old);
        assert!(Store::open(&path).is_err());

        let db = raw(&path);
        assert_eq!(version(&db).unwrap(), 0);
        assert_eq!(legacy_json(&db).unwrap().as_deref(), Some(unreadable));
        let tables: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name='snapshot' OR type='trigger'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
        // The previous binary can still use its data after the failed upgrade.
        let (mut old, _) = legacy::open(&path).unwrap();
        old.commit(&legacy_state()).unwrap();
    }

    #[test]
    fn restored_legacy_backup_is_migrated_and_guarded_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = legacy_profile(dir.path());
        Store::open(&path).unwrap();
        let [backup] = backups(dir.path()).try_into().unwrap();
        // Restore the closed-app copy, as a user following the backup procedure would.
        for suffix in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        std::fs::copy(&backup, &path).unwrap();
        assert_eq!(version(&raw(&path)).unwrap(), 0);

        let store = Store::open(&path).unwrap();
        assert_eq!(store.snapshot().cursor, 42);
        assert_eq!(version(&raw(&path)).unwrap(), DATABASE_VERSION);
        assert!(legacy::open(&path).is_err());
        assert_eq!(backups(dir.path()).len(), 2);
    }
}
