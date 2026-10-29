//! Platform-independent task storage and Astraea Sync protocol.
pub mod alarms;
pub mod backup;
pub mod entries;
pub mod identity;
pub mod migrations;
pub mod schedule;
pub mod sync;

use rusqlite::{Connection, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, String>;

/// The newest task schema this version understands; newer ones are kept read-only (see
/// `Record::unsupported`), because a schema change can change behaviour, not just add display
/// fields. 1: reminders. 2: adds entry types (roadmap 5a), so far markers.
pub const SCHEMA_VERSION: u32 = 2;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub schema_version: u32,
    pub title: String,
    pub notes: String,
    pub completed: bool,
    /// UTC Unix milliseconds. None means no reminder.
    pub remind_at: Option<i64>,
    /// Schema 2. Absent means a reminder, as every schema 1 task is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<entries::Kind>,
    /// Schema 2 markers: a local date (`YYYY-MM-DD`) that never shifts with the time zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    /// Schema 2: how long it takes, shown with the entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_minutes: Option<u32>,
    /// Schema 2: one repeat rule. Ticking an occurrence rolls the task forward to the next one
    /// (see `Task::rolled`), so every device sees the series move on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<entries::Repeat>,
    /// Schema 2: the first occurrence the repeat counts from (Unix milliseconds; local midnight
    /// for markers), so month ends and leap days recover instead of drifting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_from: Option<i64>,
    /// Schema 2: the IANA zone calendar repeats and marker warnings follow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
    /// Schema 2: minutes before each occurrence to warn. For markers, before 09:00 on the day,
    /// so 0 is a "Day of" warning.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<u32>,
    /// Fields added by newer app versions within the same schema. They are additive by
    /// definition (a behavioural change needs a new schema version), so they are kept and sent
    /// back unchanged.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Task {
    pub fn kind(&self) -> entries::Kind {
        self.kind.unwrap_or(entries::Kind::Reminder)
    }

    /// The lowest schema that can hold this task. Plain reminders stay at schema 1 so clients
    /// that only know schema 1 can keep editing them.
    pub fn needed_schema(&self) -> u32 {
        if self.kind() != entries::Kind::Reminder
            || self.date.is_some()
            || self.duration_minutes.is_some()
            || self.repeat.is_some()
            || self.repeat_from.is_some()
            || self.time_zone.is_some()
            || !self.warnings.is_empty()
        {
            2
        } else {
            1
        }
    }

    fn zone(&self) -> Result<jiff::tz::TimeZone> {
        match &self.time_zone {
            Some(zone) => {
                jiff::tz::TimeZone::get(zone).map_err(|_| format!("Unknown time zone {zone}."))
            }
            None => Ok(jiff::tz::TimeZone::system()),
        }
    }

    /// Local midnight of a marker's date in the task's zone, in Unix milliseconds.
    fn marker_start(&self) -> Result<i64> {
        let date: jiff::civil::Date = self
            .date
            .as_deref()
            .ok_or("Markers need a date.")?
            .parse()
            .map_err(|_| "Invalid marker date.")?;
        Ok(date
            .to_zoned(self.zone()?)
            .map_err(|e| e.to_string())?
            .timestamp()
            .as_millisecond())
    }

    /// What warnings count back from: the reminder time, or 09:00 local on a marker's day.
    pub(crate) fn warn_base(&self) -> Result<Option<i64>> {
        if self.kind() != entries::Kind::Marker {
            return Ok(self.remind_at);
        }
        let date: jiff::civil::Date = self
            .date
            .as_deref()
            .ok_or("Markers need a date.")?
            .parse()
            .map_err(|_| "Invalid marker date.")?;
        Ok(Some(
            date.at(9, 0, 0, 0)
                .to_zoned(self.zone()?)
                .map_err(|e| e.to_string())?
                .timestamp()
                .as_millisecond(),
        ))
    }

    /// The occurrence this task currently stands for: its time, or a marker's day.
    fn current(&self) -> Result<i64> {
        if self.kind() == entries::Kind::Marker {
            self.marker_start()
        } else {
            self.remind_at
                .ok_or_else(|| "This task has no time.".into())
        }
    }

    /// The recurrence engine's view of this task, counted from `repeat_from`.
    fn entry(&self) -> Result<entries::Entry> {
        let marker = self.kind() == entries::Kind::Marker;
        let anchor = match self.repeat_from {
            Some(anchor) => anchor,
            None => self.current()?,
        };
        let when = if marker {
            let start = jiff::Timestamp::from_millisecond(anchor)
                .map_err(|e| e.to_string())?
                .to_zoned(self.zone()?)
                .date()
                .to_string();
            entries::When::Days { start, end: None }
        } else {
            entries::When::At { at: anchor }
        };
        Ok(entries::Entry {
            schema_version: entries::SCHEMA,
            kind: if marker {
                entries::Kind::Marker
            } else {
                entries::Kind::Reminder
            },
            title: self.title.clone(),
            notes: String::new(),
            completed: false,
            when,
            time_zone: self.time_zone.clone().unwrap_or_else(|| "UTC".into()),
            duration_minutes: None,
            repeat: self.repeat.clone(),
            warnings: vec![],
        })
    }

    /// This task moved on to the occurrence after the current one, open again, or None when the
    /// series has no further occurrence within two years.
    pub fn rolled(&self) -> Result<Option<Task>> {
        let current = self.current()?;
        let window = (current + 1, current + 2 * 366 * 86_400_000);
        let mut next = self
            .entry()?
            .occurrences(window.0, window.1)?
            .into_iter()
            .next();
        if next.is_none() && self.repeat_from.is_some() {
            // Very old anchors can exceed the engine's step bound; count from now instead.
            let mut recent = self.clone();
            recent.repeat_from = Some(current);
            next = recent
                .entry()?
                .occurrences(window.0, window.1)?
                .into_iter()
                .next();
        }
        let zone = self.zone()?;
        Ok(next.map(|(_, at)| {
            let mut task = self.clone();
            if self.kind() == entries::Kind::Marker {
                task.date = Some(at.with_time_zone(zone).date().to_string());
            } else {
                task.remind_at = Some(at.timestamp().as_millisecond());
            }
            task.completed = false;
            task
        }))
    }

    /// The next `count` occurrences from the current one (Unix milliseconds; local midnight for
    /// markers), for previews. A task without a repeat has just the one.
    pub fn upcoming(&self, count: usize) -> Result<Vec<i64>> {
        let mut task = self.clone();
        if task.repeat.is_some() && task.repeat_from.is_none() {
            task.repeat_from = Some(task.current()?);
        }
        let mut out = vec![task.current()?];
        while out.len() < count && task.repeat.is_some() {
            let Some(next) = task.rolled()? else { break };
            task = next;
            out.push(task.current()?);
        }
        Ok(out)
    }

    pub fn validate(&self) -> Result<()> {
        if !(1..=SCHEMA_VERSION).contains(&self.schema_version) {
            return Err("Unsupported task schema; update the app before syncing.".into());
        }
        if self.schema_version < self.needed_schema() {
            return Err("This task needs task schema 2.".into());
        }
        match self.kind() {
            entries::Kind::Reminder if self.date.is_some() => {
                return Err("Reminders have a time, not a date.".into());
            }
            entries::Kind::Marker => {
                let date = self.date.as_deref().ok_or("Markers need a date.")?;
                date.parse::<jiff::civil::Date>()
                    .map_err(|_| "Invalid marker date.")?;
                if self.remind_at.is_some() {
                    return Err("Markers have a date, not a time.".into());
                }
            }
            // Ringing until stopped needs its own native path (roadmap 5a).
            entries::Kind::Alarm => return Err("Alarms are not available yet.".into()),
            _ => {}
        }
        if self
            .duration_minutes
            .is_some_and(|m| m == 0 || m > 7 * 24 * 60)
        {
            return Err("Durations run from a minute to a week.".into());
        }
        if self.warnings.len() > 8 || self.warnings.iter().any(|&w| w > 31 * 24 * 60) {
            return Err("Use up to 8 warnings, each at most a month before.".into());
        }
        if let Some(zone) = &self.time_zone {
            jiff::tz::TimeZone::get(zone).map_err(|_| format!("Unknown time zone {zone}."))?;
        }
        if let Some(repeat) = &self.repeat {
            if self.time_zone.is_none() {
                return Err("Repeats need a time zone.".into());
            }
            let marker = self.kind() == entries::Kind::Marker;
            if !marker && self.remind_at.is_none() {
                return Err("A repeating reminder needs a time.".into());
            }
            self.entry()?.validate()?;
            if marker && matches!(repeat, entries::Repeat::Fixed { .. }) {
                return Err("Markers repeat by days, weeks, months or years.".into());
            }
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

/// A payload this version cannot interpret, kept verbatim so it is never lost or overwritten.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Opaque {
    pub value: Option<serde_json::Value>,
    pub version: u64,
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
    /// The latest server value was written by a newer app version in a schema this one does not
    /// support. The record is then read-only here: it is not uploaded, edited or scheduled, and
    /// `task` keeps the last value this version understood, for display. A pending local edit
    /// keeps its old base version, so a newer client sees it as a conflict instead of it
    /// overwriting newer data.
    #[serde(default)]
    pub unsupported: Option<Opaque>,
}

impl Record {
    /// Whether this version may change the record.
    pub fn editable(&self) -> bool {
        self.unsupported.is_none()
    }

    /// The newest server version this device has seen for the record.
    pub(crate) fn seen(&self) -> u64 {
        self.version
            .max(self.unsupported.as_ref().map_or(0, |u| u.version))
    }

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
    /// Synced records whose keys are not task IDs, such as types added by newer app versions.
    /// Kept so the cursor can advance past them; never uploaded or shown.
    #[serde(default)]
    pub foreign: BTreeMap<String, Opaque>,
    /// The sync account this profile belongs to, from its identity marker (see `identity`).
    #[serde(default)]
    pub account: Option<String>,
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
        let stored = id
            .as_ref()
            .and_then(|id| self.state.records.get(id))
            .and_then(|r| r.task.clone());
        if task.repeat.is_none() {
            task.repeat_from = None;
        } else if stored.as_ref().is_none_or(|s| {
            s.repeat != task.repeat || s.remind_at != task.remind_at || s.date != task.date
        }) {
            // A new or changed series counts from the occurrence being saved.
            task.repeat_from = Some(task.current()?);
        }
        // Ticking an occurrence of a repeating task moves it on to the next one.
        if task.completed && task.repeat.is_some() && stored.as_ref().is_some_and(|s| !s.completed)
        {
            task.schema_version = task.needed_schema();
            if let Some(next) = task.rolled()? {
                task = next;
            }
        }
        task.schema_version = task.needed_schema();
        task.validate()?;
        let mut state = self.snapshot();
        let id = id.unwrap_or_else(|| Uuid::new_v4().to_string());
        if Uuid::parse_str(&id).is_err() {
            return Err("Invalid task ID.".into());
        }
        let record = state.records.entry(id.clone()).or_default();
        if !record.editable() {
            return Err("This task was changed by a newer version of NothingToDo. Update the app to edit it.".into());
        }
        // Editors only know this version's fields; keep any additive ones they left out.
        if let Some(stored) = &record.task {
            for (key, value) in &stored.extra {
                task.extra.entry(key).or_insert_with(|| value.clone());
            }
        }
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
        if !record.editable() {
            return Err("This task was changed by a newer version of NothingToDo. Update the app to edit it.".into());
        }
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
        if !record.editable() {
            return Err("This task was changed by a newer version of NothingToDo. Update the app to edit it.".into());
        }
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
            .filter(|(_, record)| record.editable())
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
        if !record.editable() {
            return Err("This task was changed by a newer version of NothingToDo. Update the app to edit it.".into());
        }
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
            kind: None,
            date: None,
            duration_minutes: None,
            repeat: None,
            repeat_from: None,
            time_zone: None,
            warnings: vec![],
            extra: Default::default(),
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
    fn saving_picks_the_lowest_schema_and_markers_need_a_date() {
        let mut store = Store::open(":memory:").unwrap();
        let id = store.save_task(None, task()).unwrap();
        assert_eq!(
            store.snapshot().records[&id]
                .task
                .as_ref()
                .unwrap()
                .schema_version,
            1
        );

        let mut marker = task();
        marker.remind_at = None;
        marker.kind = Some(entries::Kind::Marker);
        marker.date = Some("2026-12-03".into());
        store.save_task(Some(id.clone()), marker.clone()).unwrap();
        let saved = store.snapshot().records[&id].task.clone().unwrap();
        assert_eq!(saved.schema_version, 2);
        assert_eq!(serde_json::to_value(&saved).unwrap()["kind"], "marker");
        // Back to a plain reminder: schema 1 again, so older clients can edit it.
        store.save_task(Some(id.clone()), task()).unwrap();
        assert_eq!(
            store.snapshot().records[&id]
                .task
                .as_ref()
                .unwrap()
                .schema_version,
            1
        );

        let mut timed = marker.clone();
        timed.remind_at = Some(5);
        assert!(
            store.save_task(None, timed).is_err(),
            "markers have no time"
        );
        let mut undated = marker.clone();
        undated.date = None;
        assert!(store.save_task(None, undated).is_err());
        let mut bad = marker.clone();
        bad.date = Some("2026-02-30".into());
        assert!(store.save_task(None, bad).is_err());
        let mut alarm = task();
        alarm.kind = Some(entries::Kind::Alarm);
        assert!(
            store.save_task(None, alarm).is_err(),
            "alarms wait for their ringing path"
        );
        // A schema 1 payload cannot smuggle in schema 2 fields.
        let mut sneaky = marker;
        sneaky.schema_version = 1;
        assert!(sneaky.validate().is_err());
        // Markers are never scheduled as notifications on their day.
        assert!(
            store
                .alarms(0)
                .unwrap()
                .iter()
                .all(|a| a.task_id != id || a.at != 0)
        );
    }

    fn ms(text: &str) -> i64 {
        text.parse::<jiff::Timestamp>().unwrap().as_millisecond()
    }

    fn repeating(at: &str, repeat: entries::Repeat) -> Task {
        let mut t = task();
        t.remind_at = Some(ms(at));
        t.repeat = Some(repeat);
        t.time_zone = Some("Europe/London".into());
        t
    }

    fn monthly() -> entries::Repeat {
        entries::Repeat::Calendar {
            unit: entries::Unit::Month,
            every: 1,
            weekdays: vec![],
        }
    }

    #[test]
    fn ticking_a_repeating_task_rolls_it_to_the_next_occurrence() {
        let mut store = Store::open(":memory:").unwrap();
        let id = store
            .save_task(None, repeating("2027-01-31T09:00:00Z", monthly()))
            .unwrap();
        let current = |store: &Store| store.snapshot().records[&id].task.clone().unwrap();
        assert_eq!(current(&store).schema_version, 2);
        assert_eq!(
            current(&store).repeat_from,
            Some(ms("2027-01-31T09:00:00Z"))
        );
        for expected in ["2027-02-28T09:00:00Z", "2027-03-31T08:00:00Z"] {
            let mut done = current(&store);
            done.completed = true;
            store.save_task(Some(id.clone()), done).unwrap();
            let now = current(&store);
            // Counted from the anchor, so 31 March comes back after February; local 09:00 is
            // 08:00 UTC once summer time starts.
            assert!(!now.completed);
            assert_eq!(now.remind_at, Some(ms(expected)));
        }
        // Moving the time starts the series again from there.
        let mut moved = current(&store);
        moved.remind_at = Some(ms("2027-04-10T09:00:00Z"));
        store.save_task(Some(id.clone()), moved).unwrap();
        assert_eq!(
            current(&store).repeat_from,
            Some(ms("2027-04-10T09:00:00Z"))
        );
        // Ending the repeat and ticking completes it for good.
        let mut end = current(&store);
        end.repeat = None;
        end.completed = true;
        store.save_task(Some(id.clone()), end).unwrap();
        assert!(current(&store).completed && current(&store).repeat_from.is_none());
    }

    #[test]
    fn notification_complete_rolls_a_repeating_task_too() {
        let mut store = Store::open(":memory:").unwrap();
        let daily = entries::Repeat::Calendar {
            unit: entries::Unit::Day,
            every: 1,
            weekdays: vec![],
        };
        let id = store
            .save_task(None, repeating("2026-11-02T07:00:00Z", daily))
            .unwrap();
        let action = alarms::NotificationAction::Complete;
        assert!(
            store
                .apply_notification_action(&id, ms("2026-11-02T07:00:00Z"), &action, 0)
                .unwrap()
        );
        assert_eq!(
            store.snapshot().records[&id]
                .task
                .as_ref()
                .unwrap()
                .remind_at,
            Some(ms("2026-11-03T07:00:00Z"))
        );
    }

    #[test]
    fn yearly_markers_roll_by_date() {
        let mut store = Store::open(":memory:").unwrap();
        let mut birthday = task();
        birthday.remind_at = None;
        birthday.kind = Some(entries::Kind::Marker);
        birthday.date = Some("2026-12-03".into());
        birthday.time_zone = Some("Europe/London".into());
        birthday.repeat = Some(entries::Repeat::Calendar {
            unit: entries::Unit::Year,
            every: 1,
            weekdays: vec![],
        });
        let id = store.save_task(None, birthday).unwrap();
        let mut done = store.snapshot().records[&id].task.clone().unwrap();
        done.completed = true;
        store.save_task(Some(id.clone()), done).unwrap();
        let now = store.snapshot().records[&id].task.clone().unwrap();
        assert_eq!(
            (now.date.as_deref(), now.completed),
            (Some("2027-12-03"), false)
        );
    }

    #[test]
    fn upcoming_previews_a_series() {
        let weekdays = entries::Repeat::Calendar {
            unit: entries::Unit::Day,
            every: 1,
            weekdays: vec![1, 2, 3, 4, 5],
        };
        // Thursday 29 October 2026, 07:00 UTC (London is on GMT by then).
        let next = repeating("2026-10-29T07:00:00Z", weekdays)
            .upcoming(3)
            .unwrap();
        assert_eq!(
            next,
            [
                ms("2026-10-29T07:00:00Z"),
                ms("2026-10-30T07:00:00Z"),
                ms("2026-11-02T07:00:00Z")
            ]
        );
        assert_eq!(task().upcoming(3).unwrap(), [1000]);
    }

    #[test]
    fn repeat_rules_are_validated() {
        let mut store = Store::open(":memory:").unwrap();
        let mut no_zone = repeating("2027-01-31T09:00:00Z", monthly());
        no_zone.time_zone = None;
        assert!(store.save_task(None, no_zone).is_err());
        let mut no_time = repeating("2027-01-31T09:00:00Z", monthly());
        no_time.remind_at = None;
        assert!(store.save_task(None, no_time).is_err());
        let mut fixed_marker = task();
        fixed_marker.remind_at = None;
        fixed_marker.kind = Some(entries::Kind::Marker);
        fixed_marker.date = Some("2026-12-03".into());
        fixed_marker.time_zone = Some("UTC".into());
        fixed_marker.repeat = Some(entries::Repeat::Fixed { minutes: 8 * 60 });
        assert!(store.save_task(None, fixed_marker).is_err());
    }

    #[test]
    fn warnings_get_their_own_notifications() {
        let mut store = Store::open(":memory:").unwrap();
        let mut dentist = task();
        dentist.title = "Dentist".into();
        dentist.remind_at = Some(ms("2026-11-10T09:00:00Z"));
        dentist.warnings = vec![24 * 60, 30];
        let id = store.save_task(None, dentist).unwrap();
        let alarms = store.alarms(ms("2026-11-01T00:00:00Z")).unwrap();
        let shown: Vec<_> = alarms
            .iter()
            .map(|a| (a.at, a.title.as_str(), a.warning))
            .collect();
        assert_eq!(
            shown,
            [
                (ms("2026-11-09T09:00:00Z"), "Dentist · in 1 day", true),
                (ms("2026-11-10T08:30:00Z"), "Dentist · in 30 min", true),
                (ms("2026-11-10T09:00:00Z"), "Dentist", false),
            ]
        );
        let ids: std::collections::BTreeSet<_> = alarms.iter().map(|a| a.notification_id).collect();
        assert_eq!(ids.len(), 3);
        assert!(alarms.iter().all(|a| a.task_id == id));
        // Desktop delivers warnings as their time passes.
        let due = store.warnings_between(ms("2026-11-09T08:59:00Z"), ms("2026-11-09T09:00:00Z"));
        assert_eq!(due, [(id.clone(), "Dentist · in 1 day".to_string())]);

        let mut bins = task();
        bins.title = "Bins".into();
        bins.remind_at = None;
        bins.kind = Some(entries::Kind::Marker);
        bins.date = Some("2026-11-12".into());
        bins.time_zone = Some("Europe/London".into());
        bins.warnings = vec![0];
        store.save_task(None, bins).unwrap();
        let day_of = store.alarms(ms("2026-11-11T00:00:00Z")).unwrap();
        assert!(
            day_of
                .iter()
                .any(|a| a.title == "Bins · today" && a.at == ms("2026-11-12T09:00:00Z"))
        );
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
