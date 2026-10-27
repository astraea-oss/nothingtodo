//! Scheduling policy for the native sync worker, using elapsed time for deterministic tests.
use std::time::Duration;

#[derive(Default)]
pub struct SyncSchedule {
    due: Option<Duration>,
    failures: u32,
}

impl SyncSchedule {
    pub fn wake(&mut self, now: Duration) {
        self.due = Some(now);
    }

    pub fn edited(&mut self, now: Duration) {
        // Preserve retry backoff during outages, even if edits keep arriving.
        if self.failures > 0 {
            return;
        }
        let due = now + Duration::from_millis(750);
        self.due = Some(self.due.map_or(due, |existing| existing.min(due)));
    }

    pub fn succeeded(&mut self, now: Duration) {
        self.failures = 0;
        self.due = Some(now + Duration::from_secs(30));
    }

    pub fn failed(&mut self, now: Duration) -> u64 {
        let seconds = (5_u64 << self.failures.min(6)).min(300);
        self.failures = self.failures.saturating_add(1);
        self.due = Some(now + Duration::from_secs(seconds));
        seconds
    }

    pub fn delay(&self, now: Duration) -> Option<Duration> {
        self.due.map(|due| due.saturating_sub(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edits_coalesce_without_postponing_an_existing_sync() {
        let mut schedule = SyncSchedule::default();
        assert_eq!(schedule.delay(Duration::ZERO), None);
        schedule.edited(Duration::ZERO);
        schedule.edited(Duration::from_millis(500));
        assert_eq!(
            schedule.delay(Duration::from_millis(500)),
            Some(Duration::from_millis(250))
        );
        schedule.wake(Duration::from_millis(600));
        assert_eq!(
            schedule.delay(Duration::from_millis(600)),
            Some(Duration::ZERO)
        );
    }
    #[test]
    fn retry_backoff_is_bounded_and_edits_cannot_bypass_it() {
        let mut schedule = SyncSchedule::default();
        for expected in [5, 10, 20, 40, 80, 160, 300, 300] {
            assert_eq!(schedule.failed(Duration::ZERO), expected);
            schedule.edited(Duration::from_secs(1));
            assert_eq!(
                schedule.delay(Duration::ZERO),
                Some(Duration::from_secs(expected))
            );
        }
        schedule.wake(Duration::from_secs(2));
        assert_eq!(schedule.delay(Duration::from_secs(2)), Some(Duration::ZERO));
        schedule.succeeded(Duration::from_secs(3));
        assert_eq!(
            schedule.delay(Duration::from_secs(3)),
            Some(Duration::from_secs(30))
        );
        assert_eq!(schedule.failed(Duration::from_secs(3)), 5);
    }
}
