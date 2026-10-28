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
    pub title: String,
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
    /// Active reminders still in the future, soonest first. Past reminders are not scheduled: they
    /// show as overdue instead, so a first sync or a long absence cannot cause a burst of
    /// notifications. Assigns notification IDs as needed and drops those of finished tasks.
    pub fn alarms(&mut self, now: i64) -> Result<Vec<Alarm>> {
        let mut upcoming: Vec<(i64, &String, &str)> = self
            .state
            .records
            .iter()
            .filter_map(|(id, record)| {
                let task = record.task.as_ref().filter(|task| !task.completed)?;
                let at = record.reminder_at().filter(|&at| at > now)?;
                Some((at, id, task.title.as_str()))
            })
            .collect();
        upcoming.sort();
        upcoming.truncate(MAX_SCHEDULED);
        let upcoming: Vec<(i64, String, String)> = upcoming
            .into_iter()
            .map(|(at, id, title)| (at, id.clone(), title.into()))
            .collect();

        let mut ids = self.state.notification_ids.clone();
        ids.assigned.retain(|task_id, _| {
            self.state
                .records
                .get(task_id)
                .is_some_and(|record| record.task.as_ref().is_some_and(|task| !task.completed))
        });
        let alarms = upcoming
            .into_iter()
            .map(|(at, task_id, title)| Alarm {
                notification_id: ids.assign(&task_id),
                task_id,
                at,
                title,
            })
            .collect();
        if ids != self.state.notification_ids {
            let mut state = self.snapshot();
            state.notification_ids = ids;
            self.commit(state)?;
        }
        Ok(alarms)
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
