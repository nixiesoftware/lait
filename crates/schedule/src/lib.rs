#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unreachable,
        clippy::unimplemented,
        clippy::unchecked_time_subtraction,
        clippy::todo,
        clippy::string_slice,
        clippy::panic_in_result_fn,
        clippy::panic,
        clippy::exit,
        clippy::as_conversions
    )
)]

//! Deterministic recurring civil-time windows.
//!
//! A window is authored as a civil datetime plus an explicit IANA time zone.
//! Calendar recurrence therefore keeps its wall-clock meaning across daylight
//! saving transitions. Consumers supply wall-clock milliseconds; the primitive
//! answers which windows are active and the exact next semantic boundary.

use std::collections::BTreeSet;
use std::fmt;

use jiff::{civil::DateTime, tz::TimeZone, Span, Timestamp, Zoned};
use serde::{Deserialize, Serialize};

pub const MAX_WINDOW_DURATION_MS: u64 = 366 * 24 * 60 * 60 * 1_000;
pub const MAX_EXCEPTIONS: usize = 256;
pub const MAX_EXPANDED_OCCURRENCES: usize = 4_096;
pub const MAX_TIMEZONE_BYTES: usize = 128;
pub const MAX_LOCAL_DATETIME_BYTES: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recurrence {
    None,
    Daily,
    Weekly,
    Monthly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OccurrenceException {
    Cancel {
        occurrence_start_unix_ms: u64,
    },
    Replace {
        occurrence_start_unix_ms: u64,
        start_local: String,
        duration_ms: u64,
    },
}

impl OccurrenceException {
    fn occurrence_start_unix_ms(&self) -> u64 {
        match self {
            Self::Cancel {
                occurrence_start_unix_ms,
            }
            | Self::Replace {
                occurrence_start_unix_ms,
                ..
            } => *occurrence_start_unix_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    /// ISO-8601 civil datetime without an offset or zone annotation.
    pub start_local: String,
    pub duration_ms: u64,
    pub recurrence: Recurrence,
    /// Inclusive limit on base occurrence starts.
    pub until_unix_ms: Option<u64>,
    pub priority: i16,
    pub enabled: bool,
    /// IANA time-zone identifier, for example `America/Chicago`.
    pub timezone: String,
    #[serde(default)]
    pub exceptions: Vec<OccurrenceException>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Occurrence {
    /// The base occurrence instant. Replacements retain this identity.
    pub scheduled_start_unix_ms: u64,
    pub start_unix_ms: u64,
    pub end_unix_ms: u64,
    pub priority: i16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    pub active: Vec<usize>,
    pub next_boundary_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Overlap {
    pub left: usize,
    pub right: usize,
    pub start_unix_ms: u64,
    pub end_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid(String);

impl Invalid {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for Invalid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Invalid {}

impl Window {
    pub fn validate(&self) -> Result<(), Invalid> {
        let (start, _) = self.start()?;
        let start_unix_ms = timestamp_ms(&start)?;
        if self.duration_ms == 0 || self.duration_ms > MAX_WINDOW_DURATION_MS {
            return Err(Invalid::new("schedule window duration is out of bounds"));
        }
        if self.exceptions.len() > MAX_EXCEPTIONS {
            return Err(Invalid::new("schedule window has too many exceptions"));
        }
        if self
            .until_unix_ms
            .is_some_and(|until| until < start_unix_ms)
        {
            return Err(Invalid::new("schedule window ends before it starts"));
        }
        let mut identities = BTreeSet::new();
        for exception in &self.exceptions {
            let identity = exception.occurrence_start_unix_ms();
            if !identities.insert(identity) {
                return Err(Invalid::new("schedule occurrence exception is duplicated"));
            }
            if self.until_unix_ms.is_some_and(|until| identity > until) {
                return Err(Invalid::new(
                    "schedule occurrence exception is after the recurrence limit",
                ));
            }
            if !is_base_occurrence(&start, self.recurrence, identity)? {
                return Err(Invalid::new(
                    "schedule occurrence exception does not identify an occurrence",
                ));
            }
            if let OccurrenceException::Replace {
                start_local,
                duration_ms,
                ..
            } = exception
            {
                if *duration_ms == 0 || *duration_ms > MAX_WINDOW_DURATION_MS {
                    return Err(Invalid::new(
                        "replacement occurrence duration is out of bounds",
                    ));
                }
                parse_local(start_local, &self.timezone)?;
            }
        }
        Ok(())
    }

    pub fn evaluate_at(&self, now_unix_ms: u64) -> Result<(bool, Option<u64>), Invalid> {
        self.validate()?;
        if !self.enabled {
            return Ok((false, None));
        }
        let longest_duration = self
            .exceptions
            .iter()
            .filter_map(|exception| match exception {
                OccurrenceException::Replace { duration_ms, .. } => Some(*duration_ms),
                OccurrenceException::Cancel { .. } => None,
            })
            .fold(self.duration_ms, u64::max);
        let range_start = now_unix_ms.saturating_sub(longest_duration);
        let range_end = if matches!(self.recurrence, Recurrence::None) {
            let (start, zone) = self.start()?;
            let mut end = timestamp_ms(&start)?
                .checked_add(self.duration_ms)
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| Invalid::new("schedule boundary range overflowed"))?;
            for exception in &self.exceptions {
                if let OccurrenceException::Replace {
                    start_local,
                    duration_ms,
                    ..
                } = exception
                {
                    end = end.max(
                        timestamp_ms(&parse_local_with_zone(start_local, &zone)?)?
                            .checked_add(*duration_ms)
                            .and_then(|value| value.checked_add(1))
                            .ok_or_else(|| Invalid::new("schedule boundary range overflowed"))?,
                    );
                }
            }
            end
        } else {
            now_unix_ms
                .checked_add(MAX_WINDOW_DURATION_MS)
                .ok_or_else(|| Invalid::new("schedule boundary range overflowed"))?
        };
        let occurrences = self.occurrences_between(range_start, range_end)?;
        let mut active = false;
        let mut next = None;
        for occurrence in occurrences {
            if occurrence.start_unix_ms <= now_unix_ms && now_unix_ms < occurrence.end_unix_ms {
                active = true;
            }
            for boundary in [occurrence.start_unix_ms, occurrence.end_unix_ms] {
                if boundary > now_unix_ms {
                    next = Some(next.map_or(boundary, |current: u64| current.min(boundary)));
                }
            }
        }
        Ok((active, next))
    }

    pub fn occurrences_between(
        &self,
        range_start_unix_ms: u64,
        range_end_unix_ms: u64,
    ) -> Result<Vec<Occurrence>, Invalid> {
        self.validate()?;
        if !self.enabled || range_end_unix_ms <= range_start_unix_ms {
            return Ok(Vec::new());
        }
        let (start, zone) = self.start()?;
        let mut occurrences = Vec::new();
        let threshold = range_start_unix_ms.saturating_sub(self.duration_ms);
        let first_index = approximate_index(&start, threshold, self.recurrence)?;
        let mut index = first_index;
        let mut emitted = 0usize;
        loop {
            if emitted >= MAX_EXPANDED_OCCURRENCES {
                return Err(Invalid::new("schedule expansion exceeds its bound"));
            }
            let Some(base) = occurrence_at(&start, self.recurrence, index)? else {
                break;
            };
            let scheduled_start = timestamp_ms(&base)?;
            if self
                .until_unix_ms
                .is_some_and(|until| scheduled_start > until)
            {
                break;
            }
            if scheduled_start >= range_end_unix_ms {
                break;
            }
            if let Some(exception) = self.exception(scheduled_start) {
                if let OccurrenceException::Replace {
                    start_local,
                    duration_ms,
                    ..
                } = exception
                {
                    let replacement = parse_local_with_zone(start_local, &zone)?;
                    push_if_intersecting(
                        &mut occurrences,
                        scheduled_start,
                        timestamp_ms(&replacement)?,
                        *duration_ms,
                        self.priority,
                        range_start_unix_ms,
                        range_end_unix_ms,
                    )?;
                }
            } else {
                push_if_intersecting(
                    &mut occurrences,
                    scheduled_start,
                    scheduled_start,
                    self.duration_ms,
                    self.priority,
                    range_start_unix_ms,
                    range_end_unix_ms,
                )?;
            }
            emitted = emitted
                .checked_add(1)
                .ok_or_else(|| Invalid::new("schedule occurrence count overflowed"))?;
            if matches!(self.recurrence, Recurrence::None) {
                break;
            }
            index = index
                .checked_add(1)
                .ok_or_else(|| Invalid::new("schedule recurrence index overflowed"))?;
        }

        // A replacement can move outside the base occurrence's search range.
        // Consider every bounded exception independently, while retaining the
        // scheduled base instant as its stable identity.
        for exception in &self.exceptions {
            let OccurrenceException::Replace {
                occurrence_start_unix_ms,
                start_local,
                duration_ms,
            } = exception
            else {
                continue;
            };
            if occurrences
                .iter()
                .any(|occurrence| occurrence.scheduled_start_unix_ms == *occurrence_start_unix_ms)
            {
                continue;
            }
            let replacement = parse_local_with_zone(start_local, &zone)?;
            push_if_intersecting(
                &mut occurrences,
                *occurrence_start_unix_ms,
                timestamp_ms(&replacement)?,
                *duration_ms,
                self.priority,
                range_start_unix_ms,
                range_end_unix_ms,
            )?;
        }
        occurrences.sort_by_key(|occurrence| {
            (
                occurrence.start_unix_ms,
                occurrence.end_unix_ms,
                occurrence.scheduled_start_unix_ms,
            )
        });
        occurrences.dedup();
        Ok(occurrences)
    }

    fn start(&self) -> Result<(Zoned, TimeZone), Invalid> {
        if self.timezone.is_empty()
            || self.timezone.len() > MAX_TIMEZONE_BYTES
            || !self.timezone.is_ascii()
        {
            return Err(Invalid::new("schedule timezone is invalid"));
        }
        let zone = TimeZone::get(&self.timezone)
            .map_err(|error| Invalid::new(format!("unknown schedule timezone: {error}")))?;
        let start = parse_local_with_zone(&self.start_local, &zone)?;
        timestamp_ms(&start)?;
        Ok((start, zone))
    }

    fn exception(&self, scheduled_start_unix_ms: u64) -> Option<&OccurrenceException> {
        self.exceptions
            .iter()
            .find(|exception| exception.occurrence_start_unix_ms() == scheduled_start_unix_ms)
    }
}

pub fn evaluate(windows: &[Window], now_unix_ms: u64) -> Result<Evaluation, Invalid> {
    let mut active = Vec::new();
    let mut next_boundary = None;
    for (index, window) in windows.iter().enumerate() {
        let (is_active, next) = window.evaluate_at(now_unix_ms)?;
        if is_active {
            active.push(index);
        }
        if let Some(next) = next {
            next_boundary = Some(next_boundary.map_or(next, |current: u64| current.min(next)));
        }
    }
    Ok(Evaluation {
        active,
        next_boundary_unix_ms: next_boundary,
    })
}

pub fn overlaps_between(
    windows: &[Window],
    range_start_unix_ms: u64,
    range_end_unix_ms: u64,
) -> Result<Vec<Overlap>, Invalid> {
    let mut expanded = Vec::with_capacity(windows.len());
    for window in windows {
        expanded.push(window.occurrences_between(range_start_unix_ms, range_end_unix_ms)?);
    }
    let mut overlaps = Vec::new();
    for (left, left_occurrences) in expanded.iter().enumerate() {
        for (right, right_occurrences) in expanded.iter().enumerate().skip(left.saturating_add(1)) {
            for a in left_occurrences {
                for b in right_occurrences {
                    let start = a.start_unix_ms.max(b.start_unix_ms);
                    let end = a.end_unix_ms.min(b.end_unix_ms);
                    if start < end {
                        overlaps.push(Overlap {
                            left,
                            right,
                            start_unix_ms: start,
                            end_unix_ms: end,
                        });
                    }
                }
            }
        }
    }
    overlaps.sort_by_key(|overlap| {
        (
            overlap.start_unix_ms,
            overlap.end_unix_ms,
            overlap.left,
            overlap.right,
        )
    });
    Ok(overlaps)
}

fn parse_local(value: &str, timezone: &str) -> Result<Zoned, Invalid> {
    let zone = TimeZone::get(timezone)
        .map_err(|error| Invalid::new(format!("unknown schedule timezone: {error}")))?;
    parse_local_with_zone(value, &zone)
}

fn parse_local_with_zone(value: &str, zone: &TimeZone) -> Result<Zoned, Invalid> {
    if value.is_empty() || value.len() > MAX_LOCAL_DATETIME_BYTES || !value.is_ascii() {
        return Err(Invalid::new("schedule local datetime is invalid"));
    }
    let local: DateTime = value
        .parse()
        .map_err(|error| Invalid::new(format!("invalid schedule local datetime: {error}")))?;
    zone.to_ambiguous_zoned(local)
        .compatible()
        .map_err(|error| Invalid::new(format!("resolve schedule local datetime: {error}")))
}

fn timestamp_ms(value: &Zoned) -> Result<u64, Invalid> {
    u64::try_from(value.timestamp().as_millisecond())
        .map_err(|_| Invalid::new("schedule instants must be at or after the Unix epoch"))
}

fn approximate_index(
    start: &Zoned,
    threshold_unix_ms: u64,
    recurrence: Recurrence,
) -> Result<u64, Invalid> {
    if matches!(recurrence, Recurrence::None) {
        return Ok(0);
    }
    let start_ms = timestamp_ms(start)?;
    if threshold_unix_ms <= start_ms {
        return Ok(0);
    }
    let threshold_i64 = i64::try_from(threshold_unix_ms)
        .map_err(|_| Invalid::new("schedule threshold is out of range"))?;
    let threshold = Timestamp::from_millisecond(threshold_i64)
        .map_err(|error| Invalid::new(format!("schedule threshold is invalid: {error}")))?
        .to_zoned(start.time_zone().clone());
    let approximate = match recurrence {
        Recurrence::None => 0,
        Recurrence::Daily => threshold_unix_ms.saturating_sub(start_ms) / 86_400_000,
        Recurrence::Weekly => threshold_unix_ms.saturating_sub(start_ms) / 604_800_000,
        Recurrence::Monthly => {
            let years = i64::from(threshold.year())
                .checked_sub(i64::from(start.year()))
                .ok_or_else(|| Invalid::new("schedule year difference overflowed"))?;
            let months = years
                .checked_mul(12)
                .and_then(|value| value.checked_add(i64::from(threshold.month())))
                .and_then(|value| value.checked_sub(i64::from(start.month())))
                .ok_or_else(|| Invalid::new("schedule month difference overflowed"))?;
            u64::try_from(months.max(0))
                .map_err(|_| Invalid::new("schedule month difference is out of range"))?
        }
    };
    Ok(approximate.saturating_sub(3))
}

fn is_base_occurrence(
    start: &Zoned,
    recurrence: Recurrence,
    candidate_unix_ms: u64,
) -> Result<bool, Invalid> {
    let first = approximate_index(start, candidate_unix_ms, recurrence)?;
    for offset in 0..8 {
        let index = first
            .checked_add(offset)
            .ok_or_else(|| Invalid::new("schedule recurrence index overflowed"))?;
        let Some(occurrence) = occurrence_at(start, recurrence, index)? else {
            return Ok(false);
        };
        let occurrence_unix_ms = timestamp_ms(&occurrence)?;
        if occurrence_unix_ms == candidate_unix_ms {
            return Ok(true);
        }
        if occurrence_unix_ms > candidate_unix_ms {
            return Ok(false);
        }
    }
    Ok(false)
}

fn occurrence_at(
    start: &Zoned,
    recurrence: Recurrence,
    index: u64,
) -> Result<Option<Zoned>, Invalid> {
    if matches!(recurrence, Recurrence::None) && index > 0 {
        return Ok(None);
    }
    let index = i64::try_from(index)
        .map_err(|_| Invalid::new("schedule recurrence index is out of range"))?;
    let span = match recurrence {
        Recurrence::None => Ok(Span::new()),
        Recurrence::Daily => Span::new().try_days(index),
        Recurrence::Weekly => Span::new().try_weeks(index),
        Recurrence::Monthly => Span::new().try_months(index),
    }
    .map_err(|error| Invalid::new(format!("schedule recurrence is out of range: {error}")))?;
    start
        .checked_add(span)
        .map(Some)
        .map_err(|error| Invalid::new(format!("expand schedule recurrence: {error}")))
}

#[allow(clippy::too_many_arguments)]
fn push_if_intersecting(
    occurrences: &mut Vec<Occurrence>,
    scheduled_start_unix_ms: u64,
    start_unix_ms: u64,
    duration_ms: u64,
    priority: i16,
    range_start_unix_ms: u64,
    range_end_unix_ms: u64,
) -> Result<(), Invalid> {
    let end_unix_ms = start_unix_ms
        .checked_add(duration_ms)
        .ok_or_else(|| Invalid::new("schedule occurrence end overflowed"))?;
    if start_unix_ms < range_end_unix_ms && end_unix_ms > range_start_unix_ms {
        occurrences.push(Occurrence {
            scheduled_start_unix_ms,
            start_unix_ms,
            end_unix_ms,
            priority,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn millis(value: &str) -> u64 {
        u64::try_from(value.parse::<Timestamp>().unwrap().as_millisecond()).unwrap()
    }

    fn daily() -> Window {
        Window {
            start_local: "2024-03-09T09:00:00".into(),
            duration_ms: 60 * 60 * 1_000,
            recurrence: Recurrence::Daily,
            until_unix_ms: None,
            priority: 0,
            enabled: true,
            timezone: "America/Chicago".into(),
            exceptions: Vec::new(),
        }
    }

    #[test]
    fn recurrence_is_civil_time_and_boundaries_are_exact() {
        let window = daily();
        let before = millis("2024-03-10T13:59:59.999Z");
        let evaluation = evaluate(std::slice::from_ref(&window), before).unwrap();
        assert!(evaluation.active.is_empty());
        assert_eq!(
            evaluation.next_boundary_unix_ms,
            Some(millis("2024-03-10T14:00:00Z"))
        );

        let active = evaluate(&[window], millis("2024-03-10T14:30:00Z")).unwrap();
        assert_eq!(active.active, vec![0]);
        assert_eq!(
            active.next_boundary_unix_ms,
            Some(millis("2024-03-10T15:00:00Z"))
        );

        let far_future = Window {
            start_local: "2030-01-01T00:00:00".into(),
            duration_ms: 1_000,
            recurrence: Recurrence::None,
            until_unix_ms: None,
            priority: 0,
            enabled: true,
            timezone: "UTC".into(),
            exceptions: Vec::new(),
        };
        let evaluation = evaluate(&[far_future], millis("2024-01-01T00:00:00Z")).unwrap();
        assert_eq!(
            evaluation.next_boundary_unix_ms,
            Some(millis("2030-01-01T00:00:00Z"))
        );
    }

    #[test]
    fn monthly_recurrence_constrains_the_authored_day() {
        let window = Window {
            start_local: "2024-01-31T09:00:00".into(),
            duration_ms: 1_000,
            recurrence: Recurrence::Monthly,
            until_unix_ms: None,
            priority: 0,
            enabled: true,
            timezone: "UTC".into(),
            exceptions: Vec::new(),
        };
        let occurrences = window
            .occurrences_between(
                millis("2024-02-01T00:00:00Z"),
                millis("2024-03-01T00:00:00Z"),
            )
            .unwrap();
        assert_eq!(occurrences.len(), 1);
        assert_eq!(occurrences[0].start_unix_ms, millis("2024-02-29T09:00:00Z"));
    }

    #[test]
    fn occurrence_exceptions_cancel_or_move_one_instance() {
        let mut window = daily();
        window.exceptions = vec![OccurrenceException::Replace {
            occurrence_start_unix_ms: millis("2024-03-10T14:00:00Z"),
            start_local: "2024-03-10T11:00:00".into(),
            duration_ms: 30 * 60 * 1_000,
        }];
        let occurrences = window
            .occurrences_between(
                millis("2024-03-10T00:00:00Z"),
                millis("2024-03-11T00:00:00Z"),
            )
            .unwrap();
        assert_eq!(occurrences.len(), 1);
        assert_eq!(occurrences[0].start_unix_ms, millis("2024-03-10T16:00:00Z"));

        window.exceptions = vec![OccurrenceException::Cancel {
            occurrence_start_unix_ms: millis("2024-03-10T14:00:00.001Z"),
        }];
        assert!(window.validate().is_err());
    }

    #[test]
    fn overlap_check_includes_an_occurrence_that_started_before_the_range() {
        let left = Window {
            start_local: "2024-03-09T23:00:00".into(),
            duration_ms: 3 * 60 * 60 * 1_000,
            recurrence: Recurrence::None,
            until_unix_ms: None,
            priority: 0,
            enabled: true,
            timezone: "UTC".into(),
            exceptions: Vec::new(),
        };
        let right = Window {
            start_local: "2024-03-10T01:00:00".into(),
            duration_ms: 2 * 60 * 60 * 1_000,
            recurrence: Recurrence::None,
            until_unix_ms: None,
            priority: 1,
            enabled: true,
            timezone: "UTC".into(),
            exceptions: Vec::new(),
        };
        let overlaps = overlaps_between(
            &[left, right],
            millis("2024-03-10T00:00:00Z"),
            millis("2024-03-11T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(overlaps.len(), 1);
        assert_eq!(overlaps[0].start_unix_ms, millis("2024-03-10T01:00:00Z"));
        assert_eq!(overlaps[0].end_unix_ms, millis("2024-03-10T02:00:00Z"));
    }
}
