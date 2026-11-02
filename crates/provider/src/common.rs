//! Common logic shared across TZif providers

use crate::{
    epoch_nanoseconds::EpochNanoseconds,
    provider::{GapEntryOffsets, TimeZoneProviderResult, UtcOffsetSeconds},
    utils,
};

use core::{cmp::Ordering, ops::Range};

#[cfg(feature = "tzif")]
use tzif::data::{
    posix::{DstTransitionInfo, PosixTzString, TransitionDay},
    tzif::LocalTimeTypeRecord,
};

// TODO: Workshop record name?
/// The `LocalTimeRecord` result represents the result of searching for a
/// time zone transition without the offset seconds applied to the
/// epoch seconds.
///
/// As a result of the search, it is possible for the resulting search to be either
/// Empty (due to an invalid time being provided that would be in the +1 tz shift)
/// or two time zones (when a time exists in the ambiguous range of a -1 shift).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalTimeRecordResult {
    Empty(GapEntryOffsets),
    Single(UtcOffsetSeconds),
    Ambiguous {
        first: UtcOffsetSeconds,
        second: UtcOffsetSeconds,
    },
}

/// `TimeZoneTransitionInfo` represents information about a timezone transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeZoneTransitionInfo {
    /// The transition time epoch at which the offset needs to be applied.
    pub transition_epoch: Option<i64>,
    /// The time zone offset in seconds.
    pub offset: UtcOffsetSeconds,
}

impl From<UtcOffsetSeconds> for LocalTimeRecordResult {
    fn from(value: UtcOffsetSeconds) -> Self {
        Self::Single(value)
    }
}

#[cfg(feature = "tzif")]
impl From<LocalTimeTypeRecord> for LocalTimeRecordResult {
    fn from(value: LocalTimeTypeRecord) -> Self {
        Self::Single(value.into())
    }
}

#[cfg(feature = "tzif")]
impl From<(LocalTimeTypeRecord, LocalTimeTypeRecord)> for LocalTimeRecordResult {
    fn from(value: (LocalTimeTypeRecord, LocalTimeTypeRecord)) -> Self {
        Self::Ambiguous {
            first: value.0.into(),
            second: value.1.into(),
        }
    }
}

/// Day specification in a POSIX TZ transition rule.
#[derive(Debug, Clone, Copy)]
pub(crate) enum PosixTransitionDay {
    /// `Jn`: 1-based Julian day (1..=365), Feb 29 is never counted.
    JulianNoLeap(u16),
    /// `n`: 0-based Julian day (0..=365), Feb 29 is counted in leap years.
    JulianWithLeap(u16),
    /// `Mm.w.d`: month (1..=12), week (1..=5), day of week (0..=6, Sunday=0).
    Mwd(u16, u16, u16),
}

#[cfg(feature = "tzif")]
impl From<TransitionDay> for PosixTransitionDay {
    fn from(value: TransitionDay) -> Self {
        match value {
            TransitionDay::NoLeap(day) => Self::JulianNoLeap(day),
            TransitionDay::WithLeap(day) => Self::JulianWithLeap(day),
            TransitionDay::Mwd(month, week, day) => Self::Mwd(month, week, day),
        }
    }
}

/// A single resolved POSIX transition with its UTC epoch timestamp and surrounding offsets.
#[derive(Debug, Clone, Copy)]
struct PosixTransition {
    /// Offset in effect before this transition.
    prev: UtcOffsetSeconds,
    /// Offset in effect at and after this transition.
    next: UtcOffsetSeconds,
    /// UTC epoch seconds at which the transition occurs.
    transition_time: i64,
}

impl PosixTransition {
    fn prev_local(&self) -> i64 {
        self.transition_time + self.prev.0
    }

    fn next_local(&self) -> i64 {
        self.transition_time + self.next.0
    }

    fn kind(&self) -> TransitionKind {
        match self.prev.0.cmp(&self.next.0) {
            Ordering::Less => TransitionKind::Gap,
            Ordering::Greater => TransitionKind::Overlap,
            Ordering::Equal => TransitionKind::Smooth,
        }
    }

    fn offset_range_local(&self) -> Range<i64> {
        let prev = self.prev_local();
        let next = self.next_local();
        match self.kind() {
            TransitionKind::Overlap => next..prev,
            _ => prev..next,
        }
    }

    fn record_for_contains(&self) -> LocalTimeRecordResult {
        match self.kind() {
            TransitionKind::Gap => LocalTimeRecordResult::Empty(GapEntryOffsets {
                offset_before: self.prev,
                offset_after: self.next,
                transition_epoch: EpochNanoseconds::from_seconds(self.transition_time),
            }),
            TransitionKind::Overlap => LocalTimeRecordResult::Ambiguous {
                first: self.prev,
                second: self.next,
            },
            TransitionKind::Smooth => LocalTimeRecordResult::Single(self.prev),
        }
    }
}

/// Stores the information about DST transitions for a given year
pub(crate) struct DstTransitionInfoForYear {
    pub(crate) dst_start_seconds: i64,
    pub(crate) dst_end_seconds: i64,
    pub(crate) std_offset: UtcOffsetSeconds,
    pub(crate) dst_offset: UtcOffsetSeconds,
}

impl DstTransitionInfoForYear {
    /// Maximum seconds a POSIX transition (time in `-167:59:59..=+167:59:59` plus max UTC offset
    /// difference) could spill across an ISO year boundary. 15 days is a generous upper bound.
    const BOUNDARY_WINDOW_SECONDS: i64 = 15 * 86_400;

    #[cfg(feature = "tzif")]
    pub(crate) fn compute(
        posix_tz_string: &PosixTzString,
        dst_variant: &DstTransitionInfo,
        year: i32,
    ) -> Self {
        let std_offset = UtcOffsetSeconds::from(&posix_tz_string.std_info);
        let dst_offset = UtcOffsetSeconds::from(&dst_variant.variant_info);
        let dst_start_seconds = calculate_transition_seconds_for_year(
            year,
            dst_variant.start_date.day.into(),
            dst_variant.start_date.time.0,
            std_offset,
        );
        let dst_end_seconds = calculate_transition_seconds_for_year(
            year,
            dst_variant.end_date.day.into(),
            dst_variant.end_date.time.0,
            dst_offset,
        );
        Self {
            dst_start_seconds,
            dst_end_seconds,
            std_offset,
            dst_offset,
        }
    }

    // Returns the range between offsets in this year
    // This may cover DST or standard time, whichever starts first
    pub(crate) fn transition_range(&self) -> Range<i64> {
        if self.dst_start_seconds > self.dst_end_seconds {
            self.dst_end_seconds..self.dst_start_seconds
        } else {
            self.dst_start_seconds..self.dst_end_seconds
        }
    }

    /// The offset in effect immediately after the transition at `seconds`.
    pub(crate) fn offset_after(&self, seconds: i64) -> UtcOffsetSeconds {
        if seconds == self.dst_start_seconds {
            self.dst_offset
        } else {
            self.std_offset
        }
    }

    /// Returns this year's two transitions in chronological UTC order.
    fn ordered_transitions(&self) -> (PosixTransition, PosixTransition) {
        let start = PosixTransition {
            prev: self.std_offset,
            next: self.dst_offset,
            transition_time: self.dst_start_seconds,
        };
        let end = PosixTransition {
            prev: self.dst_offset,
            next: self.std_offset,
            transition_time: self.dst_end_seconds,
        };
        if self.dst_start_seconds <= self.dst_end_seconds {
            (start, end)
        } else {
            (end, start)
        }
    }

    /// Resolves the candidate local time record(s) for `local_seconds` (wall-clock seconds since
    /// `1970-01-01T00:00:00`) using `compute_for_year` to compute POSIX transitions per year.
    pub(crate) fn resolve_local_seconds(
        local_seconds: i64,
        compute_for_year: impl Fn(i32) -> TimeZoneProviderResult<Self>,
    ) -> TimeZoneProviderResult<LocalTimeRecordResult> {
        let year = utils::epoch_time_to_iso_year(local_seconds * 1_000);
        let (first, second) = compute_for_year(year)?.ordered_transitions();
        let first_range = first.offset_range_local();

        if local_seconds < first_range.start {
            let year_start_seconds = i64::from(utils::epoch_days_for_year(year)) * 86_400;
            if local_seconds - year_start_seconds < Self::BOUNDARY_WINDOW_SECONDS {
                let (prev_first, prev_second) = compute_for_year(year - 1)?.ordered_transitions();
                let prev_second_range = prev_second.offset_range_local();
                if prev_second_range.contains(&local_seconds) {
                    return Ok(prev_second.record_for_contains());
                } else if local_seconds < prev_second_range.start {
                    let prev_first_range = prev_first.offset_range_local();
                    if prev_first_range.contains(&local_seconds) {
                        return Ok(prev_first.record_for_contains());
                    } else if local_seconds < prev_first_range.start {
                        return Ok(LocalTimeRecordResult::Single(prev_first.prev));
                    }
                    return Ok(LocalTimeRecordResult::Single(prev_second.prev));
                }
            }
            return Ok(LocalTimeRecordResult::Single(first.prev));
        }

        if first_range.contains(&local_seconds) {
            return Ok(first.record_for_contains());
        }

        let second_range = second.offset_range_local();
        if local_seconds < second_range.start {
            return Ok(LocalTimeRecordResult::Single(first.next));
        }

        if second_range.contains(&local_seconds) {
            return Ok(second.record_for_contains());
        }

        let next_year_start_seconds = i64::from(utils::epoch_days_for_year(year + 1)) * 86_400;
        if next_year_start_seconds - local_seconds < Self::BOUNDARY_WINDOW_SECONDS {
            let (next_first, next_second) = compute_for_year(year + 1)?.ordered_transitions();
            let next_first_range = next_first.offset_range_local();
            if next_first_range.contains(&local_seconds) {
                return Ok(next_first.record_for_contains());
            } else if local_seconds >= next_first_range.end {
                let next_second_range = next_second.offset_range_local();
                if next_second_range.contains(&local_seconds) {
                    return Ok(next_second.record_for_contains());
                } else if local_seconds >= next_second_range.end {
                    return Ok(LocalTimeRecordResult::Single(next_second.next));
                }
                return Ok(LocalTimeRecordResult::Single(next_first.next));
            }
        }

        Ok(LocalTimeRecordResult::Single(second.next))
    }

    /// Resolves the active offset and preceding transition epoch for exact UTC `epoch_seconds`.
    pub(crate) fn resolve_epoch_seconds(
        epoch_seconds: i64,
        compute_for_year: impl Fn(i32) -> TimeZoneProviderResult<Self>,
    ) -> TimeZoneProviderResult<TimeZoneTransitionInfo> {
        let year = utils::epoch_time_to_iso_year(epoch_seconds * 1_000);
        let (first, second) = compute_for_year(year)?.ordered_transitions();

        if epoch_seconds < first.transition_time {
            let (prev_first, prev_second) = compute_for_year(year - 1)?.ordered_transitions();
            if epoch_seconds < prev_first.transition_time {
                let (_, prev2_second) = compute_for_year(year - 2)?.ordered_transitions();
                return Ok(TimeZoneTransitionInfo {
                    offset: prev_first.prev,
                    transition_epoch: Some(prev2_second.transition_time),
                });
            }
            if epoch_seconds < prev_second.transition_time {
                return Ok(TimeZoneTransitionInfo {
                    offset: prev_second.prev,
                    transition_epoch: Some(prev_first.transition_time),
                });
            }
            return Ok(TimeZoneTransitionInfo {
                offset: first.prev,
                transition_epoch: Some(prev_second.transition_time),
            });
        }

        if epoch_seconds < second.transition_time {
            return Ok(TimeZoneTransitionInfo {
                offset: first.next,
                transition_epoch: Some(first.transition_time),
            });
        }

        let next_year_start_seconds = i64::from(utils::epoch_days_for_year(year + 1)) * 86_400;
        if next_year_start_seconds - epoch_seconds < Self::BOUNDARY_WINDOW_SECONDS {
            let (next_first, next_second) = compute_for_year(year + 1)?.ordered_transitions();
            if epoch_seconds >= next_second.transition_time {
                return Ok(TimeZoneTransitionInfo {
                    offset: next_second.next,
                    transition_epoch: Some(next_second.transition_time),
                });
            }
            if epoch_seconds >= next_first.transition_time {
                return Ok(TimeZoneTransitionInfo {
                    offset: next_first.next,
                    transition_epoch: Some(next_first.transition_time),
                });
            }
        }

        Ok(TimeZoneTransitionInfo {
            offset: second.next,
            transition_epoch: Some(second.transition_time),
        })
    }
}

pub(crate) fn calculate_transition_seconds_for_year(
    year: i32,
    transition_day: PosixTransitionDay,
    transition_time_seconds: i64,
    offset: UtcOffsetSeconds,
) -> i64 {
    // Determine the year of the requested time.
    let year_epoch_seconds = i64::from(utils::epoch_days_for_year(year)) * 86400;
    let is_leap = utils::is_leap(year);

    // Calculate the days in the year for the TransitionDate
    // This value is zero-indexed so it can be added to the year's epoch seconds
    let days = match transition_day {
        PosixTransitionDay::JulianNoLeap(day) if day > 59 => day - 1 + is_leap as u16,
        PosixTransitionDay::JulianNoLeap(day) => day - 1,
        PosixTransitionDay::JulianWithLeap(day) => day,
        PosixTransitionDay::Mwd(month, week, day) => {
            let days_to_month = utils::month_to_day((month - 1) as u8, is_leap);
            let days_in_month = u16::from(utils::iso_days_in_month(year, month as u8));

            // Month starts in the day...
            let day_offset = (u16::from(utils::epoch_seconds_to_day_of_week(year_epoch_seconds))
                + days_to_month)
                .rem_euclid(7);

            // EXAMPLE:
            //
            // 0   1   2   3   4   5   6
            // sun mon tue wed thu fri sat
            // -   -   -   0   1   2   3
            // 4   5   6   7   8   9   10
            // 11  12  13  14  15  16  17
            // 18  19  20  21  22  23  24
            // 25  26  27  28  29  30  -
            //
            // The day_offset = 3, since the month starts on a wednesday.
            //
            // We're looking for the second friday of the month. Thus, since the month started before
            // a friday, we need to start counting from week 0:
            //
            // day_of_month = (week - u16::from(day_offset <= day)) * 7 + day - day_offset = (2 - 1) * 7 + 5 - 3 = 9
            //
            // This works if the month started on a day before the day we want (day_offset <= day). However, if that's not the
            // case, we need to start counting on week 1. For example, calculate the day of the month for the third monday
            // of the month:
            //
            // day_of_month = (week - u16::from(day_offset <= day)) * 7 + day - day_offset = (3 - 0) * 7 + 1 - 3 = 19

            // Note: this day_of_month is zero-indexed!
            let mut day_of_month = (week - u16::from(day_offset <= day)) * 7 + day - day_offset;

            // Week 5 actually means "last <dayofweek> of month". The day_of_month calculation
            // above uses `week` directly; so we might end up spilling into the next month. In that
            // case, we normalize to the fourth week of the month.
            //
            // Note that this only needs to be done once; if a month will have at least four of each
            // day of the week since all months have 28 days or greater.
            //
            // We add one because day_of_month is zero_indexed
            if day_of_month + 1 > days_in_month {
                day_of_month -= 7
            }

            days_to_month + day_of_month
        }
    };

    // Transition time is on local time, so we need to add the UTC offset to get the correct UTC timestamp
    // for the transition.
    year_epoch_seconds + i64::from(days) * 86400 + transition_time_seconds - offset.0
}

#[derive(Debug)]
pub(crate) enum TransitionKind {
    // The offsets didn't change (happens when abbreviations/savings values change)
    Smooth,
    // The offsets changed in a way that leaves a gap
    Gap,
    // The offsets changed in a way that produces overlapping time.
    Overlap,
}

#[cfg(test)]
mod tests {
    use super::{
        calculate_transition_seconds_for_year, DstTransitionInfoForYear, LocalTimeRecordResult,
        PosixTransitionDay, TimeZoneTransitionInfo,
    };
    use crate::{
        epoch_nanoseconds::EpochNanoseconds,
        provider::{GapEntryOffsets, UtcOffsetSeconds},
        utils,
    };

    fn local_secs(year: i32, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> i64 {
        let epoch_days = utils::epoch_days_from_gregorian_date(year, month, day);
        epoch_days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second)
    }

    #[allow(clippy::too_many_arguments)]
    fn make_year_info(
        year: i32,
        std_offset: i64,
        dst_offset: i64,
        start_day: PosixTransitionDay,
        start_time: i64,
        end_day: PosixTransitionDay,
        end_time: i64,
    ) -> DstTransitionInfoForYear {
        let std_offset = UtcOffsetSeconds(std_offset);
        let dst_offset = UtcOffsetSeconds(dst_offset);
        DstTransitionInfoForYear {
            dst_start_seconds: calculate_transition_seconds_for_year(
                year, start_day, start_time, std_offset,
            ),
            dst_end_seconds: calculate_transition_seconds_for_year(
                year, end_day, end_time, dst_offset,
            ),
            std_offset,
            dst_offset,
        }
    }

    #[test]
    fn negative_epoch_seconds_day_of_week() {
        // 1970-01-01T00:00:00Z is Thursday (4)
        assert_eq!(utils::epoch_seconds_to_day_of_week(0), 4);
        // 1969-12-31T23:59:59Z and 1969-12-31T12:00:00Z are Wednesday (3)
        assert_eq!(utils::epoch_seconds_to_day_of_week(-1), 3);
        assert_eq!(utils::epoch_seconds_to_day_of_week(-43_200), 3);
        // 1969-12-31T00:00:00Z is Wednesday (3)
        assert_eq!(utils::epoch_seconds_to_day_of_week(-86_400), 3);
        // 1969-12-30T23:59:59Z is Tuesday (2)
        assert_eq!(utils::epoch_seconds_to_day_of_week(-86_401), 2);
    }

    #[test]
    fn leap_february_last_weekday_rule() {
        // Synthetic rule: UTC+0 -> UTC+1 on last Sunday of Feb (M2.5.0/2) to M10.5.0/3.
        // In 2024 (leap year, Feb has 29 days, Feb 29 is Thursday), the last Sunday of Feb is
        // Feb 25 (4th Sunday), whereas the last Thursday (M2.5.4) is Feb 29 (5th Thursday).
        let resolve_sun = |secs| {
            DstTransitionInfoForYear::resolve_local_seconds(secs, |y| {
                Ok(make_year_info(
                    y,
                    0,
                    3_600,
                    PosixTransitionDay::Mwd(2, 5, 0),
                    7_200,
                    PosixTransitionDay::Mwd(10, 5, 0),
                    10_800,
                ))
            })
            .unwrap()
        };
        assert_eq!(
            resolve_sun(local_secs(2024, 2, 24, 12, 0, 0)),
            LocalTimeRecordResult::Single(UtcOffsetSeconds(0))
        );
        assert!(matches!(
            resolve_sun(local_secs(2024, 2, 25, 2, 30, 0)),
            LocalTimeRecordResult::Empty(_)
        ));
        assert_eq!(
            resolve_sun(local_secs(2024, 2, 26, 12, 0, 0)),
            LocalTimeRecordResult::Single(UtcOffsetSeconds(3_600))
        );

        let resolve_thu = |secs| {
            DstTransitionInfoForYear::resolve_local_seconds(secs, |y| {
                Ok(make_year_info(
                    y,
                    0,
                    3_600,
                    PosixTransitionDay::Mwd(2, 5, 4),
                    7_200,
                    PosixTransitionDay::Mwd(10, 5, 0),
                    10_800,
                ))
            })
            .unwrap()
        };
        // 4th Thursday (Feb 22) is still STD; 5th Thursday (Feb 29) has the gap.
        assert_eq!(
            resolve_thu(local_secs(2024, 2, 22, 12, 0, 0)),
            LocalTimeRecordResult::Single(UtcOffsetSeconds(0))
        );
        assert_eq!(
            resolve_thu(local_secs(2024, 2, 28, 12, 0, 0)),
            LocalTimeRecordResult::Single(UtcOffsetSeconds(0))
        );
        assert!(matches!(
            resolve_thu(local_secs(2024, 2, 29, 2, 30, 0)),
            LocalTimeRecordResult::Empty(_)
        ));
    }

    #[test]
    fn inverse_dst_and_gap_transition_epoch() {
        // Rearguard Ireland POSIX rule: IST-1GMT0,M10.5.0,M3.5.0/1
        // std_offset = +3600 (IST), dst_offset = 0 (GMT).
        // Start in Oct falls back (+3600 -> 0, Overlap), End in Mar springs forward (0 -> +3600, Gap).
        let compute = |y| {
            Ok(make_year_info(
                y,
                3_600,
                0,
                PosixTransitionDay::Mwd(10, 5, 0),
                7_200,
                PosixTransitionDay::Mwd(3, 5, 0),
                3_600,
            ))
        };

        // 2026-03-29 01:30 local is in the spring-forward gap (01:00 -> 02:00)
        let mar_gap = DstTransitionInfoForYear::resolve_local_seconds(
            local_secs(2026, 3, 29, 1, 30, 0),
            compute,
        )
        .unwrap();
        assert_eq!(
            mar_gap,
            LocalTimeRecordResult::Empty(GapEntryOffsets {
                offset_before: UtcOffsetSeconds(0),
                offset_after: UtcOffsetSeconds(3_600),
                transition_epoch: EpochNanoseconds::from_seconds(local_secs(2026, 3, 29, 1, 0, 0)),
            })
        );

        // 2026-10-25 01:30 local is in the fall-back overlap (IST +1 first, GMT 0 second)
        let oct_overlap = DstTransitionInfoForYear::resolve_local_seconds(
            local_secs(2026, 10, 25, 1, 30, 0),
            compute,
        )
        .unwrap();
        assert_eq!(
            oct_overlap,
            LocalTimeRecordResult::Ambiguous {
                first: UtcOffsetSeconds(3_600),
                second: UtcOffsetSeconds(0),
            }
        );
    }

    #[test]
    fn year_boundary_spillover_and_epoch_transition_accuracy() {
        // Rule with transition spilling into next year: J60/2 .. J365/50 (Dec 31 + 50h = Jan 2 02:00)
        let compute_spill_forward = |y| {
            Ok(make_year_info(
                y,
                0,
                3_600,
                PosixTransitionDay::JulianNoLeap(60),
                7_200,
                PosixTransitionDay::JulianNoLeap(365),
                50 * 3_600,
            ))
        };
        // 2026-01-01 12:00 is still in 2025's DST because 2025's end transition is 2026-01-02 02:00!
        assert_eq!(
            DstTransitionInfoForYear::resolve_local_seconds(
                local_secs(2026, 1, 1, 12, 0, 0),
                compute_spill_forward,
            )
            .unwrap(),
            LocalTimeRecordResult::Single(UtcOffsetSeconds(3_600))
        );
        // 2026-01-02 01:30 is in the 02:00 -> 01:00 fall-back overlap!
        assert_eq!(
            DstTransitionInfoForYear::resolve_local_seconds(
                local_secs(2026, 1, 2, 1, 30, 0),
                compute_spill_forward,
            )
            .unwrap(),
            LocalTimeRecordResult::Ambiguous {
                first: UtcOffsetSeconds(3_600),
                second: UtcOffsetSeconds(0),
            }
        );
        // 2026-01-02 12:00 is now in STD (0).
        assert_eq!(
            DstTransitionInfoForYear::resolve_local_seconds(
                local_secs(2026, 1, 2, 12, 0, 0),
                compute_spill_forward,
            )
            .unwrap(),
            LocalTimeRecordResult::Single(UtcOffsetSeconds(0))
        );

        // Rule with transition spilling back into previous year: J1/-2 (Dec 31 22:00) .. J300/2
        let compute_spill_back = |y| {
            Ok(make_year_info(
                y,
                0,
                3_600,
                PosixTransitionDay::JulianNoLeap(1),
                -7_200,
                PosixTransitionDay::JulianNoLeap(300),
                7_200,
            ))
        };
        // 2025-12-31 22:30 is in 2026's spring-forward gap (22:00 -> 23:00)!
        assert!(matches!(
            DstTransitionInfoForYear::resolve_local_seconds(
                local_secs(2025, 12, 31, 22, 30, 0),
                compute_spill_back,
            )
            .unwrap(),
            LocalTimeRecordResult::Empty(_)
        ));
        assert_eq!(
            DstTransitionInfoForYear::resolve_local_seconds(
                local_secs(2025, 12, 31, 23, 30, 0),
                compute_spill_back,
            )
            .unwrap(),
            LocalTimeRecordResult::Single(UtcOffsetSeconds(3_600))
        );

        // Verify resolve_epoch_seconds in January returns the exact UTC transition_epoch from year - 1
        // (Europe/Berlin: CET-1CEST,M3.5.0/2,M10.5.0/3; Oct 29, 2028 03:00 CEST = 1856394000 UTC).
        let berlin = |y| {
            Ok(make_year_info(
                y,
                3_600,
                7_200,
                PosixTransitionDay::Mwd(3, 5, 0),
                7_200,
                PosixTransitionDay::Mwd(10, 5, 0),
                10_800,
            ))
        };
        assert_eq!(
            DstTransitionInfoForYear::resolve_epoch_seconds(
                local_secs(2029, 1, 15, 12, 0, 0),
                berlin,
            )
            .unwrap(),
            TimeZoneTransitionInfo {
                offset: UtcOffsetSeconds(3_600),
                transition_epoch: Some(1_856_394_000),
            }
        );
    }
}
