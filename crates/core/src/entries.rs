//! Entry types, recurrence and warnings (roadmap 5a), as task schema 2.
//!
//! First pass: the data model and the occurrence engine, with the time semantics fixed by the
//! roadmap. Nothing writes schema 2 yet. The rollout rule in 0b requires a published release that
//! keeps schema 2 records read-only (it already does, see `Record::unsupported`) before any device
//! writes them; `WRITES_ENABLED` stays false until then.
//!
//! Time semantics:
//! - Alarms and reminders happen at an instant. A one-off keeps that instant when travelling.
//! - Fixed repeats (for example every 8 hours or every 28 days) step exact durations from the
//!   anchor instant.
//! - Calendar repeats (daily, weekly, monthly, yearly, with optional weekdays) keep the local
//!   wall-clock time in the entry's IANA time zone, across DST changes. Each occurrence is computed
//!   from the anchor, never from the previous occurrence, so a monthly repeat from the 31st falls on
//!   the last day of shorter months and returns to the 31st afterwards. A yearly repeat from
//!   29 February falls on 28 February in other years.
//! - A local time that does not exist (skipped by DST) moves forward by the gap; one that happens
//!   twice uses the first.
//! - Markers are local dates (or date ranges) and never shift with the time zone.
use jiff::{
    Span, Timestamp, ToSpan, Zoned,
    civil::{Date, Time, Weekday},
    tz::TimeZone,
};
use serde::{Deserialize, Serialize};

/// Flip only after a release that reads schema 2 records as read-only is the published minimum
/// supported client.
pub const WRITES_ENABLED: bool = false;
pub const SCHEMA: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    /// Rings until stopped.
    Alarm,
    /// One sound, then waits as a notification.
    Reminder,
    /// A day or span of days, such as a birthday or deadline. Silent on the day itself.
    Marker,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum When {
    /// Unix milliseconds, for alarms and reminders.
    At { at: i64 },
    /// Local dates (`YYYY-MM-DD`), for markers. `end` is inclusive.
    Days { start: String, end: Option<String> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum Repeat {
    /// Exact durations from the anchor: the 8-hour and 28-day presets.
    Fixed { minutes: u32 },
    /// Local wall-clock repeats. `weekdays` (1 = Monday … 7 = Sunday) applies to daily and weekly.
    Calendar {
        unit: Unit,
        every: u16,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        weekdays: Vec<u8>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Unit {
    Day,
    Week,
    Month,
    Year,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum Warning {
    /// A notification this long before each occurrence.
    Before { minutes: u32 },
    /// Markers: a notification on the day, at this local time (minutes after midnight).
    DayOf { minutes: u16 },
}

/// A schema 2 task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub schema_version: u32,
    pub kind: Kind,
    pub title: String,
    /// The first line shows as a summary under the title.
    pub notes: String,
    pub completed: bool,
    pub when: When,
    /// IANA zone that calendar repeats and marker warnings follow, e.g. `Europe/London`.
    pub time_zone: String,
    #[serde(default)]
    pub duration_minutes: Option<u32>,
    #[serde(default)]
    pub repeat: Option<Repeat>,
    #[serde(default)]
    pub warnings: Vec<Warning>,
}

/// One notification an entry wants, for the alarm list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Due {
    /// Unix milliseconds.
    pub at: i64,
    /// Which occurrence, counted from the anchor; stable while the entry is unchanged.
    pub occurrence: u32,
    /// None for the occurrence itself, otherwise the index into `warnings`.
    pub warning: Option<usize>,
}

/// Bounds the work per expansion; the alarm list is rebuilt as occurrences pass.
pub const MAX_STEPS: u32 = 5_000;

fn weekday(n: u8) -> Option<Weekday> {
    Weekday::from_monday_one_offset(n as i8).ok()
}

impl Entry {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != SCHEMA {
            return Err("Unsupported entry schema.".into());
        }
        if self.title.trim().is_empty() || self.title.len() > 500 || self.notes.len() > 8000 {
            return Err("Use a title of 1–500 bytes and notes up to 8000 bytes.".into());
        }
        self.zone()?;
        match (&self.when, self.kind) {
            (When::At { .. }, Kind::Alarm | Kind::Reminder) => {}
            (When::Days { start, end }, Kind::Marker) => {
                let start: Date = start.parse().map_err(|_| "Invalid marker date.")?;
                if let Some(end) = end {
                    let end: Date = end.parse().map_err(|_| "Invalid marker end date.")?;
                    if end < start {
                        return Err("A marker cannot end before it starts.".into());
                    }
                }
            }
            _ => return Err("Alarms and reminders need a time; markers need a date.".into()),
        }
        match &self.repeat {
            Some(Repeat::Fixed { minutes }) if *minutes < 60 => {
                return Err("Fixed repeats must be at least an hour apart.".into());
            }
            Some(Repeat::Fixed { .. }) if self.kind == Kind::Marker => {
                return Err("Markers repeat by calendar days, weeks, months or years.".into());
            }
            Some(Repeat::Calendar {
                every,
                weekdays,
                unit,
            }) => {
                if *every == 0 {
                    return Err("Repeat every at least 1.".into());
                }
                if !weekdays.is_empty() && !matches!(unit, Unit::Day | Unit::Week) {
                    return Err("Weekdays apply to daily and weekly repeats.".into());
                }
                if weekdays.iter().any(|&d| weekday(d).is_none()) {
                    return Err("Weekdays are numbered 1 (Monday) to 7 (Sunday).".into());
                }
            }
            _ => {}
        }
        if self.kind != Kind::Marker
            && self
                .warnings
                .iter()
                .any(|w| matches!(w, Warning::DayOf { .. }))
        {
            return Err("\"Day of\" warnings are for markers.".into());
        }
        if self
            .warnings
            .iter()
            .any(|w| matches!(w, Warning::DayOf { minutes } if *minutes >= 24 * 60))
        {
            return Err("\"Day of\" warnings need a time within the day.".into());
        }
        Ok(())
    }

    fn zone(&self) -> Result<TimeZone, String> {
        TimeZone::get(&self.time_zone).map_err(|_| format!("Unknown time zone {}.", self.time_zone))
    }

    /// The anchor as a zoned date-time: the instant for timed entries, local midnight of the first
    /// day for markers.
    fn anchor(&self, zone: &TimeZone) -> Result<Zoned, String> {
        match &self.when {
            When::At { at } => Ok(Timestamp::from_millisecond(*at)
                .map_err(|e| e.to_string())?
                .to_zoned(zone.clone())),
            When::Days { start, .. } => {
                let date: Date = start.parse().map_err(|_| "Invalid marker date.")?;
                date.to_zoned(zone.clone()).map_err(|e| e.to_string())
            }
        }
    }

    /// Occurrence starts in `[from, to)`, in order, with their indexes. Bounded by `MAX_STEPS`.
    pub fn occurrences(&self, from: i64, to: i64) -> Result<Vec<(u32, Zoned)>, String> {
        let zone = self.zone()?;
        let anchor = self.anchor(&zone)?;
        let mut out = Vec::new();
        let in_range = |z: &Zoned| {
            let ms = z.timestamp().as_millisecond();
            (from..to).contains(&ms)
        };
        let Some(repeat) = &self.repeat else {
            if in_range(&anchor) {
                out.push((0, anchor));
            }
            return Ok(out);
        };
        match repeat {
            Repeat::Fixed { minutes } => {
                let step = i64::from(*minutes) * 60_000;
                let start = anchor.timestamp().as_millisecond();
                // Jump straight to the first step at or after `from`.
                let first = if from > start {
                    (from - start + step - 1) / step
                } else {
                    0
                };
                for n in first..first + i64::from(MAX_STEPS) {
                    let ms = start + n * step;
                    if ms >= to {
                        break;
                    }
                    let Ok(index) = u32::try_from(n) else { break };
                    let at = Timestamp::from_millisecond(ms).map_err(|e| e.to_string())?;
                    out.push((index, at.to_zoned(zone.clone())));
                }
            }
            Repeat::Calendar {
                unit,
                every,
                weekdays,
            } => {
                let days: Vec<Weekday> = weekdays.iter().filter_map(|&d| weekday(d)).collect();
                let local = anchor.datetime();
                let mut n: i64 = 0;
                let mut produced = 0u32;
                for _ in 0..MAX_STEPS {
                    // Each candidate is computed from the anchor, so clamped month ends recover.
                    let span: Span = match (unit, days.is_empty()) {
                        (Unit::Day, true) => (n * i64::from(*every)).days(),
                        // With weekdays, step one day at a time and keep matching days that fall
                        // in an "every Nth" period (days for daily, weeks for weekly).
                        (Unit::Day, false) | (Unit::Week, false) => n.days(),
                        (Unit::Week, true) => (n * i64::from(*every)).weeks(),
                        (Unit::Month, _) => (n * i64::from(*every)).months(),
                        (Unit::Year, _) => (n * i64::from(*every)).years(),
                    };
                    n += 1;
                    let civil = local.checked_add(span).map_err(|e| e.to_string())?;
                    if !days.is_empty() {
                        // Daily: every Nth day from the anchor. Weekly: every Nth week, counted
                        // from the Monday of the anchor's week.
                        let in_step = match unit {
                            Unit::Day => (n - 1) % i64::from(*every) == 0,
                            _ => {
                                let offset = local.date().weekday().to_monday_zero_offset();
                                let monday = local
                                    .date()
                                    .checked_sub(i64::from(offset).days())
                                    .map_err(|e| e.to_string())?;
                                let weeks = i64::from((civil.date() - monday).get_days()) / 7;
                                weeks % i64::from(*every) == 0
                            }
                        };
                        if !in_step || !days.contains(&civil.date().weekday()) {
                            continue;
                        }
                    }
                    // A skipped local time moves forward by the gap; a repeated one uses the first.
                    let zoned = civil.to_zoned(zone.clone()).map_err(|e| e.to_string())?;
                    let ms = zoned.timestamp().as_millisecond();
                    if ms >= to {
                        break;
                    }
                    let index = produced;
                    produced += 1;
                    if ms >= from {
                        out.push((index, zoned));
                    }
                }
            }
        }
        Ok(out)
    }

    /// Notifications in `[from, to)`: each occurrence (except for markers, which are silent on the
    /// day) and each of its warnings, soonest first.
    pub fn due(&self, from: i64, to: i64) -> Result<Vec<Due>, String> {
        let lead = self
            .warnings
            .iter()
            .map(|w| match w {
                Warning::Before { minutes } => i64::from(*minutes) * 60_000,
                Warning::DayOf { .. } => 0,
            })
            .max()
            .unwrap_or(0);
        // Look ahead by the longest warning so warnings for later occurrences are included.
        let mut out = Vec::new();
        for (index, start) in self.occurrences(from, to.saturating_add(lead))? {
            let at = start.timestamp().as_millisecond();
            if self.kind != Kind::Marker && (from..to).contains(&at) {
                out.push(Due {
                    at,
                    occurrence: index,
                    warning: None,
                });
            }
            for (w, warning) in self.warnings.iter().enumerate() {
                let when = match warning {
                    Warning::Before { minutes } => at - i64::from(*minutes) * 60_000,
                    Warning::DayOf { minutes } => {
                        let time = Time::new((minutes / 60) as i8, (minutes % 60) as i8, 0, 0)
                            .map_err(|e| e.to_string())?;
                        start
                            .date()
                            .to_datetime(time)
                            .to_zoned(start.time_zone().clone())
                            .map_err(|e| e.to_string())?
                            .timestamp()
                            .as_millisecond()
                    }
                };
                if (from..to).contains(&when) {
                    out.push(Due {
                        at: when,
                        occurrence: index,
                        warning: Some(w),
                    });
                }
            }
        }
        out.sort_by_key(|d| (d.at, d.occurrence, d.warning));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(text: &str) -> i64 {
        text.parse::<Timestamp>().unwrap().as_millisecond()
    }

    fn local((_, zoned): &(u32, Zoned)) -> String {
        zoned.datetime().to_string()
    }

    fn entry(at: &str, zone: &str, repeat: Option<Repeat>) -> Entry {
        Entry {
            schema_version: SCHEMA,
            kind: Kind::Reminder,
            title: "Medicine".into(),
            notes: String::new(),
            completed: false,
            when: When::At { at: ms(at) },
            time_zone: zone.into(),
            duration_minutes: None,
            repeat,
            warnings: vec![],
        }
    }

    fn daily(weekdays: Vec<u8>) -> Option<Repeat> {
        Some(Repeat::Calendar {
            unit: Unit::Day,
            every: 1,
            weekdays,
        })
    }

    #[test]
    fn calendar_repeats_keep_local_time_across_dst() {
        // 07:30 London, before the clocks go back on 25 October 2026.
        let e = entry("2026-10-23T06:30:00Z", "Europe/London", daily(vec![]));
        e.validate().unwrap();
        let days = e
            .occurrences(ms("2026-10-23T00:00:00Z"), ms("2026-10-28T00:00:00Z"))
            .unwrap();
        let times: Vec<_> = days.iter().map(local).collect();
        assert!(times.iter().all(|t| t.ends_with("T07:30:00")), "{times:?}");
        // Same wall-clock time, so the UTC instant moves by an hour.
        assert_eq!(
            days[0].1.timestamp().as_millisecond(),
            ms("2026-10-23T06:30:00Z")
        );
        assert_eq!(
            days[4].1.timestamp().as_millisecond(),
            ms("2026-10-27T07:30:00Z")
        );
    }

    #[test]
    fn fixed_repeats_are_exact_durations_through_dst() {
        let e = entry(
            "2026-10-24T22:00:00Z",
            "Europe/London",
            Some(Repeat::Fixed { minutes: 8 * 60 }),
        );
        let doses = e
            .occurrences(ms("2026-10-24T00:00:00Z"), ms("2026-10-26T00:00:00Z"))
            .unwrap();
        let gaps: Vec<i64> = doses
            .windows(2)
            .map(|w| w[1].1.timestamp().as_millisecond() - w[0].1.timestamp().as_millisecond())
            .collect();
        assert!(gaps.iter().all(|&g| g == 8 * 3_600_000), "{gaps:?}");
        // Jumping into the middle of the series keeps the indexes stable: doses fall at 22:00,
        // 06:00, 14:00 and 22:00, so the first after 13:00 on the 25th is the third (index 2).
        let later = e
            .occurrences(ms("2026-10-25T13:00:00Z"), ms("2026-10-25T23:00:00Z"))
            .unwrap();
        assert_eq!(later[0].0, 2);
        assert_eq!(
            later[0].1.timestamp().as_millisecond(),
            ms("2026-10-25T14:00:00Z")
        );
    }

    #[test]
    fn monthly_repeats_are_computed_from_the_anchor_and_recover_after_short_months() {
        let e = entry(
            "2027-01-31T09:00:00Z",
            "UTC",
            Some(Repeat::Calendar {
                unit: Unit::Month,
                every: 1,
                weekdays: vec![],
            }),
        );
        let months = e
            .occurrences(ms("2027-01-01T00:00:00Z"), ms("2027-05-01T00:00:00Z"))
            .unwrap();
        let dates: Vec<_> = months.iter().map(|(_, z)| z.date().to_string()).collect();
        assert_eq!(
            dates,
            ["2027-01-31", "2027-02-28", "2027-03-31", "2027-04-30"]
        );
    }

    #[test]
    fn yearly_from_a_leap_day_falls_on_28_february_otherwise() {
        let e = entry(
            "2028-02-29T12:00:00Z",
            "UTC",
            Some(Repeat::Calendar {
                unit: Unit::Year,
                every: 1,
                weekdays: vec![],
            }),
        );
        let years = e
            .occurrences(ms("2028-01-01T00:00:00Z"), ms("2033-01-01T00:00:00Z"))
            .unwrap();
        let dates: Vec<_> = years.iter().map(|(_, z)| z.date().to_string()).collect();
        assert_eq!(
            dates,
            [
                "2028-02-29",
                "2029-02-28",
                "2030-02-28",
                "2031-02-28",
                "2032-02-29"
            ]
        );
    }

    #[test]
    fn weekday_sets_on_daily_and_weekly_repeats() {
        // Monday 2 November 2026, weekdays only.
        let e = entry("2026-11-02T07:00:00Z", "UTC", daily(vec![1, 2, 3, 4, 5]));
        let fortnight = e
            .occurrences(ms("2026-11-02T00:00:00Z"), ms("2026-11-16T00:00:00Z"))
            .unwrap();
        assert_eq!(fortnight.len(), 10);
        assert!(
            fortnight
                .iter()
                .all(|(_, z)| !matches!(z.date().weekday(), Weekday::Saturday | Weekday::Sunday))
        );
        // Every other week on Tuesday and Thursday.
        let e = entry(
            "2026-11-02T07:00:00Z",
            "UTC",
            Some(Repeat::Calendar {
                unit: Unit::Week,
                every: 2,
                weekdays: vec![2, 4],
            }),
        );
        let month = e
            .occurrences(ms("2026-11-01T00:00:00Z"), ms("2026-12-01T00:00:00Z"))
            .unwrap();
        let dates: Vec<_> = month.iter().map(|(_, z)| z.date().to_string()).collect();
        assert_eq!(
            dates,
            ["2026-11-03", "2026-11-05", "2026-11-17", "2026-11-19"]
        );
    }

    #[test]
    fn skipped_local_times_move_forward_by_the_gap() {
        // 01:30 does not exist in London on 28 March 2027 (clocks go from 01:00 to 02:00).
        let e = entry("2027-03-27T01:30:00Z", "Europe/London", daily(vec![]));
        let days = e
            .occurrences(ms("2027-03-27T00:00:00Z"), ms("2027-03-30T00:00:00Z"))
            .unwrap();
        let times: Vec<_> = days.iter().map(local).collect();
        assert_eq!(
            times,
            [
                "2027-03-27T01:30:00",
                "2027-03-28T02:30:00",
                "2027-03-29T01:30:00"
            ]
        );
    }

    #[test]
    fn warnings_and_silent_markers() {
        let mut e = entry("2026-11-10T09:00:00Z", "UTC", None);
        e.warnings = vec![
            Warning::Before { minutes: 24 * 60 },
            Warning::Before { minutes: 5 },
        ];
        let due = e
            .due(ms("2026-11-01T00:00:00Z"), ms("2026-12-01T00:00:00Z"))
            .unwrap();
        let at: Vec<_> = due.iter().map(|d| (d.at, d.warning)).collect();
        assert_eq!(
            at,
            [
                (ms("2026-11-09T09:00:00Z"), Some(0)),
                (ms("2026-11-10T08:55:00Z"), Some(1)),
                (ms("2026-11-10T09:00:00Z"), None),
            ]
        );
        // A warning for an occurrence just after the window is still found.
        let early = e
            .due(ms("2026-11-09T00:00:00Z"), ms("2026-11-10T00:00:00Z"))
            .unwrap();
        assert_eq!(early.len(), 1);

        let birthday = Entry {
            kind: Kind::Marker,
            when: When::Days {
                start: "2026-12-03".into(),
                end: None,
            },
            time_zone: "Europe/London".into(),
            repeat: Some(Repeat::Calendar {
                unit: Unit::Year,
                every: 1,
                weekdays: vec![],
            }),
            warnings: vec![
                Warning::DayOf { minutes: 9 * 60 },
                Warning::Before {
                    minutes: 7 * 24 * 60,
                },
            ],
            ..entry("2026-12-03T00:00:00Z", "UTC", None)
        };
        birthday.validate().unwrap();
        let due = birthday
            .due(ms("2026-11-01T00:00:00Z"), ms("2027-01-01T00:00:00Z"))
            .unwrap();
        // No notification for the day itself: a week before at midnight, and 09:00 on the day.
        assert_eq!(
            due.iter().map(|d| d.warning).collect::<Vec<_>>(),
            [Some(1), Some(0)]
        );
        assert_eq!(due[1].at, ms("2026-12-03T09:00:00Z"));
    }

    #[test]
    fn validation_rejects_mismatched_shapes() {
        let mut e = entry("2026-11-10T09:00:00Z", "Mars/Olympus", None);
        assert!(e.validate().is_err());
        e.time_zone = "UTC".into();
        e.kind = Kind::Marker;
        assert!(e.validate().is_err(), "markers need dates");
        e.kind = Kind::Alarm;
        e.repeat = Some(Repeat::Fixed { minutes: 5 });
        assert!(e.validate().is_err());
        e.repeat = Some(Repeat::Calendar {
            unit: Unit::Month,
            every: 1,
            weekdays: vec![1],
        });
        assert!(e.validate().is_err());
        e.repeat = None;
        e.warnings = vec![Warning::DayOf { minutes: 60 }];
        assert!(e.validate().is_err());
        const { assert!(!WRITES_ENABLED) };
    }

    #[test]
    fn entries_round_trip_as_json() {
        let mut e = entry("2026-11-10T09:00:00Z", "Europe/London", daily(vec![1, 5]));
        e.warnings = vec![Warning::Before { minutes: 10 }];
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["schemaVersion"], 2);
        assert_eq!(json["repeat"]["type"], "calendar");
        assert_eq!(serde_json::from_value::<Entry>(json).unwrap(), e);
    }
}
