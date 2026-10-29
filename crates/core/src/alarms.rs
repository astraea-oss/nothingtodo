//! The reminders this device should have scheduled with the operating system. Platform adapters
//! compare this list with what the OS has pending and cancel or schedule the difference.
use crate::{Result, Store};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// At most this many reminders are handed to the OS at once. Android caps pending alarms per app;
/// later reminders are scheduled as earlier ones pass and the list is reconciled again.
pub const MAX_SCHEDULED: usize = 64;

/// A reminder to schedule with the OS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alarm {
    pub notification_id: i32,
    pub task_id: String,
    /// Milliseconds since the Unix epoch; the reminder time or a later local snooze.
    pub at: i64,
    /// The synced reminder time, which notification actions quote back so outdated ones can be
    /// recognised.
    pub remind_at: i64,
    pub title: String,
    /// A warning before the entry rather than the entry itself; it has no Complete or Snooze.
    pub warning: bool,
    /// An alarm: it rings until stopped rather than showing one notification.
    pub ring: bool,
}

struct Planned {
    key: String,
    task_id: String,
    at: i64,
    remind_at: i64,
    title: String,
    warning: bool,
    ring: bool,
}

/// "45 min", "2 h", "1 day", "1 week" for warning texts.
fn span(minutes: u32) -> String {
    match minutes {
        m if m % (7 * 1440) == 0 => plural(m / (7 * 1440), "week"),
        m if m % 1440 == 0 => plural(m / 1440, "day"),
        m if m % 60 == 0 => format!("{} h", m / 60),
        m => format!("{m} min"),
    }
}

fn plural(n: u32, unit: &str) -> String {
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

/// A button pressed on a reminder notification, recorded by the OS side while the app may not
/// be running and applied when it next is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "action")]
pub enum NotificationAction {
    Complete,
    /// `until` is fixed when the button is pressed, so applying it twice changes nothing.
    Snooze {
        until: i64,
    },
}

/// Device-local mapping from task UUIDs to the 32-bit IDs notification APIs need. IDs are handed
/// out in sequence and never reused while assigned, so two tasks cannot share one.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationIds {
    pub assigned: BTreeMap<String, i32>,
    pub next: i32,
}

impl NotificationIds {
    fn assign(&mut self, task_id: &str) -> i32 {
        if let Some(&id) = self.assigned.get(task_id) {
            return id;
        }
        let mut id = self.next.max(1);
        // After two billion assignments, wrap and skip IDs still in use.
        while self.assigned.values().any(|&used| used == id) {
            id = id.checked_add(1).unwrap_or(1);
        }
        self.next = id.checked_add(1).unwrap_or(1);
        self.assigned.insert(task_id.into(), id);
        id
    }
}

impl Store {
    /// Every notification the active tasks want, in any order: each reminder at its time (or a
    /// later local snooze) and each warning before it. Markers notify only through warnings,
    /// counted back from 09:00 on their day. `key` identifies the notification across rebuilds.
    fn planned(&self) -> Vec<Planned> {
        let mut out = Vec::new();
        for (id, record) in &self.state.records {
            // A newer schema may schedule differently; leave it to the app that understands it.
            let Some(task) = record
                .task
                .as_ref()
                .filter(|t| !t.completed && record.editable())
            else {
                continue;
            };
            let marker = task.kind() == crate::entries::Kind::Marker;
            if !marker && let (Some(at), Some(remind_at)) = (record.reminder_at(), task.remind_at) {
                out.push(Planned {
                    key: id.clone(),
                    task_id: id.clone(),
                    at,
                    remind_at,
                    title: task.title.clone(),
                    warning: false,
                    ring: task.kind() == crate::entries::Kind::Alarm,
                });
            }
            let Ok(Some(base)) = task.warn_base() else {
                continue;
            };
            for (i, &minutes) in task.warnings.iter().enumerate() {
                let label = if marker && minutes == 0 {
                    "today".to_string()
                } else {
                    format!("in {}", span(minutes))
                };
                out.push(Planned {
                    key: format!("{id}/{i}"),
                    task_id: id.clone(),
                    at: base - i64::from(minutes) * 60_000,
                    remind_at: task.remind_at.unwrap_or(base),
                    title: format!("{} · {label}", task.title),
                    warning: true,
                    ring: false,
                });
            }
        }
        out
    }

    /// Warnings whose time falls in `(from, to]`, for desktop delivery from the running app.
    pub fn warnings_between(&self, from: i64, to: i64) -> Vec<(String, String)> {
        let mut due: Vec<_> = self
            .planned()
            .into_iter()
            .filter(|p| p.warning && p.at > from && p.at <= to)
            .collect();
        due.sort_by_key(|p| p.at);
        due.into_iter().map(|p| (p.task_id, p.title)).collect()
    }

    /// Notifications still in the future, soonest first and capped at `MAX_SCHEDULED`. Past ones
    /// are not scheduled: they show as overdue instead, so a first sync or a long absence cannot
    /// cause a burst of notifications. Assigns notification IDs as needed and drops those of
    /// finished tasks.
    pub fn alarms(&mut self, now: i64) -> Result<Vec<Alarm>> {
        let mut upcoming: Vec<Planned> =
            self.planned().into_iter().filter(|p| p.at > now).collect();
        upcoming.sort_by(|a, b| (a.at, &a.key).cmp(&(b.at, &b.key)));
        upcoming.truncate(MAX_SCHEDULED);

        let mut ids = self.state.notification_ids.clone();
        ids.assigned.retain(|key, _| {
            let task_id = key.split('/').next().unwrap_or(key);
            self.state
                .records
                .get(task_id)
                .is_some_and(|record| record.task.as_ref().is_some_and(|task| !task.completed))
        });
        let alarms = upcoming
            .into_iter()
            .map(|p| Alarm {
                notification_id: ids.assign(&p.key),
                task_id: p.task_id,
                at: p.at,
                remind_at: p.remind_at,
                title: p.title,
                warning: p.warning,
                ring: p.ring,
            })
            .collect();
        if ids != self.state.notification_ids {
            let mut state = self.snapshot();
            state.notification_ids = ids;
            self.commit(state)?;
        }
        Ok(alarms)
    }

    /// Applies a notification button press for the reminder that was due at `remind_at`. Returns
    /// whether the task changed in a way that needs syncing. Repeated or outdated presses change
    /// nothing: a deleted task stays deleted, and a snooze is dropped if the reminder has moved
    /// since, or if its time has already passed (the OS has shown it by then).
    pub fn apply_notification_action(
        &mut self,
        task_id: &str,
        remind_at: i64,
        action: &NotificationAction,
        now: i64,
    ) -> Result<bool> {
        let Some(record) = self.state.records.get(task_id).filter(|r| r.editable()) else {
            return Ok(false);
        };
        let Some(task) = record.task.as_ref().filter(|task| !task.completed) else {
            return Ok(false);
        };
        match *action {
            NotificationAction::Complete => {
                let mut done = task.clone();
                done.completed = true;
                self.save_task(Some(task_id.into()), done)?;
                Ok(true)
            }
            NotificationAction::Snooze { until } => {
                if task.remind_at == Some(remind_at) && until > now && until > remind_at {
                    let mut state = self.snapshot();
                    state.records.get_mut(task_id).unwrap().snooze = Some(crate::Snooze {
                        of: remind_at,
                        until,
                    });
                    self.commit(state)?;
                }
                Ok(false)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Task;

    fn task(title: &str, remind_at: Option<i64>) -> Task {
        Task {
            schema_version: 1,
            title: title.into(),
            notes: "".into(),
            completed: false,
            remind_at,
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
    fn schedules_future_active_reminders_with_stable_ids() {
        let mut store = Store::open(":memory:").unwrap();
        let later = store.save_task(None, task("Later", Some(3000))).unwrap();
        let sooner = store.save_task(None, task("Sooner", Some(2000))).unwrap();
        store.save_task(None, task("Past", Some(500))).unwrap();
        store.save_task(None, task("No reminder", None)).unwrap();
        let alarms = store.alarms(1000).unwrap();
        let order: Vec<_> = alarms.iter().map(|a| a.task_id.clone()).collect();
        assert_eq!(order, [sooner.clone(), later.clone()]);
        assert_ne!(alarms[0].notification_id, alarms[1].notification_id);

        // IDs survive restarts of the list and are dropped once a task is finished.
        let again = store.alarms(1000).unwrap();
        assert_eq!(again, alarms);
        let mut done = task("Sooner", Some(2000));
        done.completed = true;
        store.save_task(Some(sooner.clone()), done).unwrap();
        let after = store.alarms(1000).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].notification_id, alarms[1].notification_id);
        assert!(
            !store
                .snapshot()
                .notification_ids
                .assigned
                .contains_key(&sooner)
        );
        // A freed ID is not handed to the next task.
        let new = store.save_task(None, task("New", Some(4000))).unwrap();
        let new_id = store.alarms(1000).unwrap()[1].notification_id;
        assert_eq!(store.snapshot().notification_ids.assigned[&new], new_id);
        assert!(![alarms[0].notification_id, alarms[1].notification_id].contains(&new_id));
    }

    #[test]
    fn snoozed_reminder_is_scheduled_at_the_snooze() {
        let mut store = Store::open(":memory:").unwrap();
        let id = store.save_task(None, task("Call", Some(1000))).unwrap();
        store.snooze(&id, 1000, 5000).unwrap();
        assert_eq!(store.alarms(2000).unwrap()[0].at, 5000);
    }

    #[test]
    fn notification_actions_are_idempotent_and_ignore_outdated_snoozes() {
        let mut store = Store::open(":memory:").unwrap();
        let id = store.save_task(None, task("Call", Some(1000))).unwrap();
        let snooze = NotificationAction::Snooze { until: 1600 };
        // Pressed while the reminder was at 1000, applied later.
        assert!(
            !store
                .apply_notification_action(&id, 1000, &snooze, 1100)
                .unwrap()
        );
        assert_eq!(store.snapshot().records[&id].reminder_at(), Some(1600));
        let revision = store.snapshot().records[&id].local_revision;
        store
            .apply_notification_action(&id, 1000, &snooze, 1100)
            .unwrap();
        assert_eq!(store.snapshot().records[&id].local_revision, revision);
        // Moved on another device: an old snooze no longer applies.
        store
            .save_task(Some(id.clone()), task("Call", Some(5000)))
            .unwrap();
        store
            .apply_notification_action(&id, 1000, &NotificationAction::Snooze { until: 1700 }, 1100)
            .unwrap();
        assert_eq!(store.snapshot().records[&id].reminder_at(), Some(5000));
        // Applied after the snooze time: nothing left to delay.
        store
            .apply_notification_action(&id, 5000, &NotificationAction::Snooze { until: 5600 }, 6000)
            .unwrap();
        assert_eq!(store.snapshot().records[&id].snooze, None);

        assert!(
            store
                .apply_notification_action(&id, 5000, &NotificationAction::Complete, 6000)
                .unwrap()
        );
        let record = &store.snapshot().records[&id];
        assert!(record.task.as_ref().unwrap().completed && record.dirty);
        // A second delivery of the same press changes nothing.
        assert!(
            !store
                .apply_notification_action(&id, 5000, &NotificationAction::Complete, 6000)
                .unwrap()
        );
        store.delete_task(&id).unwrap();
        assert!(
            !store
                .apply_notification_action(&id, 5000, &NotificationAction::Complete, 6000)
                .unwrap()
        );
        assert!(
            !store
                .apply_notification_action("missing", 1, &NotificationAction::Complete, 6000)
                .unwrap()
        );
    }

    #[test]
    fn notification_actions_use_the_journal_format() {
        let complete: NotificationAction =
            serde_json::from_str(r#"{"action":"complete"}"#).unwrap();
        assert_eq!(complete, NotificationAction::Complete);
        let snooze: NotificationAction =
            serde_json::from_str(r#"{"action":"snooze","until":5}"#).unwrap();
        assert_eq!(snooze, NotificationAction::Snooze { until: 5 });
    }

    #[test]
    fn caps_the_number_handed_to_the_os() {
        let mut store = Store::open(":memory:").unwrap();
        for at in 0..(MAX_SCHEDULED as i64 + 5) {
            store.save_task(None, task("T", Some(2000 + at))).unwrap();
        }
        let alarms = store.alarms(1000).unwrap();
        assert_eq!(alarms.len(), MAX_SCHEDULED);
        assert_eq!(alarms.last().unwrap().at, 2000 + MAX_SCHEDULED as i64 - 1);
    }

    #[test]
    fn wrapped_counter_skips_ids_in_use() {
        let mut ids = NotificationIds {
            next: i32::MAX,
            ..Default::default()
        };
        assert_eq!(ids.assign("a"), i32::MAX);
        assert_eq!(ids.assign("b"), 1);
        ids.next = 1;
        assert_eq!(ids.assign("c"), 2);
    }
}
