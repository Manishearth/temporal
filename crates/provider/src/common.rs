//! Common logic shared across TZif providers

use crate::{
    epoch_nanoseconds::EpochNanoseconds,
    provider::{GapEntryOffsets, TimeZoneProviderResult, UtcOffsetSeconds},
    utils, TimeZoneProviderError,
};

use core::ops::Range;

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

/// Stores the information about DST transitions for a given year
pub(crate) struct DstTransitionInfoForYear {
    pub(crate) dst_start_seconds: i64,
    pub(crate) dst_end_seconds: i64,
    pub(crate) std_offset: UtcOffsetSeconds,
    pub(crate) dst_offset: UtcOffsetSeconds,
}

impl DstTransitionInfoForYear {
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

pub(crate) fn resolve_posix_epoch_seconds(
    start: (PosixTransitionDay, i64),
    end: (PosixTransitionDay, i64),
    std_offset: UtcOffsetSeconds,
    dst_offset: UtcOffsetSeconds,
    epoch_seconds: i64,
) -> TimeZoneTransitionInfo {
    let year = utils::epoch_time_to_iso_year(epoch_seconds * 1000);

    let dst_start_seconds =
        calculate_transition_seconds_for_year(year, start.0, start.1, std_offset);
    let dst_end_seconds = calculate_transition_seconds_for_year(year, end.0, end.1, dst_offset);

    // Need to determine if the range being tested is standard or savings time.
    let dst_is_inversed = dst_end_seconds < dst_start_seconds;

    // We have potentially to different variations of the DST start and end time.
    //
    // Northern hemisphere: dst_start -> dst_end
    // Southern hemisphere: dst_end -> dst_start
    //
    // This is primarily due to the summer / winter months of those areas.
    //
    // For the northern hemispere, we can check if the range contains the seconds. For the
    // southern hemisphere, we check if the range does no contain the value.
    let should_return_dst = (!dst_is_inversed
        && (dst_start_seconds..dst_end_seconds).contains(&epoch_seconds))
        || (dst_is_inversed && !(dst_end_seconds..dst_start_seconds).contains(&epoch_seconds));

    // Expanding on the above, the state of time zones in the year are:
    //
    // Northern hemisphere: STD -> DST -> STD
    // Southern hemisphere: DST -> STD -> DST
    //
    // This is simple for the returning the offsets, but if the seconds value falls into the first
    // available rule. However, the northern hemisphere's first STD rule and the Southern hemisphere's
    // first DST rule will have different transition times that are based in the year prior, so if the
    // requested seconds falls in that range, we calculate the transition time for the prior year.
    let (new_offset, transition_epoch) = if should_return_dst {
        let transition_epoch = if dst_is_inversed && epoch_seconds < dst_end_seconds {
            Some(calculate_transition_seconds_for_year(
                year - 1,
                start.0,
                start.1,
                dst_offset,
            ))
        } else {
            Some(dst_start_seconds)
        };
        (dst_offset, transition_epoch)
    } else {
        let transition_epoch = if !dst_is_inversed && epoch_seconds < dst_start_seconds {
            Some(calculate_transition_seconds_for_year(
                year - 1,
                end.0,
                end.1,
                std_offset,
            ))
        } else {
            Some(dst_end_seconds)
        };
        (std_offset, transition_epoch)
    };
    TimeZoneTransitionInfo {
        offset: new_offset,
        transition_epoch,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransitionType {
    Dst,
    Std,
}

impl TransitionType {
    fn invert(&mut self) {
        *self = match *self {
            Self::Dst => Self::Std,
            Self::Std => Self::Dst,
        }
    }
}

/// The month, week of month, and day of week value built into the POSIX tz string.
///
/// For more information, see the [POSIX tz string docs](https://sourceware.org/glibc/manual/2.40/html_node/Proleptic-TZ.html)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Mwd {
    month: u8,
    week: u8,
    day: u8,
}

impl Mwd {
    fn from_u16(month: u16, week: u16, day: u16) -> Self {
        Self::from_u8(
            u8::try_from(month).unwrap_or(0),
            u8::try_from(week).unwrap_or(0),
            u8::try_from(day).unwrap_or(0),
        )
    }

    fn from_u8(month: u8, week: u8, day: u8) -> Self {
        Self { month, week, day }
    }

    /// Given the day of the week of the 0th day in this month,
    /// normalize the week to being a week number (1 = first week, ...)
    /// rather than a weekday ordinal (1 = first friday, etc)
    pub(crate) fn normalize_to_week_number(&mut self, day_of_week_zeroth_day: u8) {
        if self.day <= day_of_week_zeroth_day {
            self.week += 1;
        }
    }
}

/// Represents an MWD for a given time
#[derive(Debug)]
pub(crate) struct MwdForTime {
    /// This will never have day = 5
    pub(crate) mwd: Mwd,
    /// The day of the week of the 0th day (the day before the month starts)
    pub(crate) day_of_week_zeroth_day: u8,
    /// This is the day of week of the 29th and the last day of the month,
    /// if the month has more than 28 days.
    /// Basically, this is the start and end of the "fifth $weekday of the month" period
    pub(crate) extra_days: Option<(u8, u8)>,
}

impl MwdForTime {
    #[cfg(any(feature = "tzif", feature = "experimental_tzif"))]
    pub(crate) fn from_seconds(seconds: i64) -> Self {
        let (year, month, day_of_month) = utils::ymd_from_epoch_milliseconds(seconds * 1_000);
        let week_of_month = (day_of_month - 1) / 7 + 1;
        let day_of_week = utils::epoch_seconds_to_day_of_week(seconds);
        let mut mwd = Mwd::from_u8(month, week_of_month, day_of_week);
        let days_in_month = utils::iso_days_in_month(year, month);
        let day_of_week_zeroth_day =
            (i16::from(day_of_week) - i16::from(day_of_month)).rem_euclid(7) as u8;
        mwd.normalize_to_week_number(day_of_week_zeroth_day);
        if days_in_month > 28 {
            let day_of_week_day_29 = (day_of_week_zeroth_day + 29).rem_euclid(7);
            let day_of_week_last_day = (day_of_week_zeroth_day + days_in_month).rem_euclid(7);
            Self {
                mwd,
                day_of_week_zeroth_day,
                extra_days: Some((day_of_week_day_29, day_of_week_last_day)),
            }
        } else {
            // No day 5
            Self {
                mwd,
                day_of_week_zeroth_day,
                extra_days: None,
            }
        }
    }

    /// MWDs from Posix data can contain `w=5`, which means the *last* $weekday of the month,
    /// not the 5th. For MWDs in the same month, this normalizes the 5 to the actual number of the
    /// last weekday of the month (5 or 4)
    ///
    /// Furthermore, this turns the week number into a true week number: the "second friday in March"
    /// will be turned into "the friday in the first week of March" or "the Friday in the second week of March"
    /// depending on when March starts.
    ///
    /// This normalization *only* applies to MWDs in the same month. For other MWDs, such normalization is irrelevant.
    pub(crate) fn normalize_mwd(&self, other: &mut Mwd) {
        // If we're in the same month, normalization will actually have a useful effect
        if self.mwd.month == other.month {
            // First normalize MWDs that are like "the last $weekday in the month"
            // the last $weekday in the month, we need special handling
            if other.week == 5 {
                if let Some((day_29, last_day)) = self.extra_days {
                    if day_29 <= last_day {
                        if other.day < day_29 || other.day > last_day {
                            // This day isn't found in the last week. Subtract one.
                            other.week = 4;
                        }
                    } else {
                        // The extra part of the month crosses Sunday
                        if other.day < day_29 && other.day > last_day {
                            // This day isn't found in the last week. Subtract one.
                            other.week = 4;
                        }
                    }
                } else {
                    // There is no week 5 in this month, normalize to 4
                    other.week = 4;
                }
            }

            other.normalize_to_week_number(self.day_of_week_zeroth_day);
        }
    }
}

fn offset_range(offset_one: i64, offset_two: i64) -> core::ops::Range<i64> {
    if offset_one < offset_two {
        return offset_one..offset_two;
    }
    offset_two..offset_one
}

pub(crate) fn resolve_posix_local_seconds(
    start: (PosixTransitionDay, i64),
    end: (PosixTransitionDay, i64),
    std: UtcOffsetSeconds,
    dst: UtcOffsetSeconds,
    local_seconds: i64,
) -> TimeZoneProviderResult<LocalTimeRecordResult> {
    // TODO: Resolve safety issue around utils.
    //   Using f64 is a hold over from early implementation days and should
    //   be moved away from.

    // NOTE:
    // STD -> DST == start
    // DST -> STD == end
    let (is_transition_day, mut is_dst) =
        cmp_seconds_to_transitions(start.0, end.0, local_seconds)?;
    if is_transition_day {
        let time = utils::epoch_ms_to_ms_in_day(local_seconds * 1_000) as i64 / 1_000;
        let transition_time = if is_dst == TransitionType::Dst {
            start.1
        } else {
            end.1
        };
        let transition_diff = if is_dst == TransitionType::Dst {
            dst.0 - std.0
        } else {
            std.0 - dst.0
        };
        let offset = offset_range(transition_time + transition_diff, transition_time);
        match offset.contains(&time) {
            true if is_dst == TransitionType::Dst => {
                return Ok(LocalTimeRecordResult::Empty(GapEntryOffsets {
                    offset_before: std,
                    offset_after: dst,
                    transition_epoch: EpochNanoseconds::from_seconds(transition_time),
                }));
            }
            true => {
                // Note(nekevss, manishearth): We may need to more carefully
                // handle inverse DST here.
                return Ok(LocalTimeRecordResult::Ambiguous {
                    first: dst,
                    second: std,
                });
            }
            _ => {}
        }

        // We were not contained in the transition above,
        // AND we are before it, which means we are actually in
        // the other transition!
        //
        // NOTE(Manishearth) do we need to do anything special
        // here if we end up back at the tzif transition data?
        if time < offset.start {
            is_dst.invert();
        }
    }

    match is_dst {
        TransitionType::Dst => Ok(dst.into()),
        TransitionType::Std => Ok(std.into()),
    }
}

fn cmp_seconds_to_transitions(
    start: PosixTransitionDay,
    end: PosixTransitionDay,
    seconds: i64,
) -> TimeZoneProviderResult<(bool, TransitionType)> {
    let cmp_result = match (start, end) {
        (
            PosixTransitionDay::Mwd(start_month, start_week, start_day),
            PosixTransitionDay::Mwd(end_month, end_week, end_day),
        ) => {
            let mwd = MwdForTime::from_seconds(seconds);
            let mut start = Mwd::from_u16(start_month, start_week, start_day);
            let mut end = Mwd::from_u16(end_month, end_week, end_day);

            mwd.normalize_mwd(&mut start);
            mwd.normalize_mwd(&mut end);

            let is_transition = start == mwd.mwd || end == mwd.mwd;
            let is_dst = if start > end {
                mwd.mwd < end || start <= mwd.mwd
            } else {
                start <= mwd.mwd && mwd.mwd < end
            };

            (is_transition, is_dst)
        }
        (PosixTransitionDay::JulianWithLeap(start), PosixTransitionDay::JulianWithLeap(end)) => {
            let day_in_year = utils::epoch_time_to_day_in_year(seconds * 1_000) as u16;
            let is_transition = start == day_in_year || end == day_in_year;
            let is_dst = if start > end {
                day_in_year < end || start <= day_in_year
            } else {
                start <= day_in_year && day_in_year < end
            };
            (is_transition, is_dst)
        }
        // TODO: do we need to modify the logic for leap years?
        (PosixTransitionDay::JulianNoLeap(start), PosixTransitionDay::JulianNoLeap(end)) => {
            let day_in_year = utils::epoch_time_to_day_in_year(seconds * 1_000) as u16;
            let is_transition = start == day_in_year || end == day_in_year;
            let is_dst = if start > end {
                day_in_year < end || start <= day_in_year
            } else {
                start <= day_in_year && day_in_year < end
            };
            (is_transition, is_dst)
        }
        // NOTE: The assumption here is that mismatched day types on
        // a POSIX string is an illformed string.
        _ => {
            return Err(TimeZoneProviderError::Assert(
                "Mismatched day types on a POSIX string.",
            ))
        }
    };

    match cmp_result {
        (true, dst) if dst => Ok((true, TransitionType::Dst)),
        (true, _) => Ok((true, TransitionType::Std)),
        (false, dst) if dst => Ok((false, TransitionType::Dst)),
        (false, _) => Ok((false, TransitionType::Std)),
    }
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
#[cfg(all(test, any(feature = "tzif", feature = "experimental_tzif")))]
mod tests {
    use super::{Mwd, MwdForTime};
    use crate::utils;

    #[test]
    fn mwd_ordering_matches_calendar_days_across_solar_cycle() {
        // A 28-year solar cycle (2000..=2027) covers all 7 starting weekdays for
        // 28-, 29-, 30-, and 31-day months (including leap-year Februaries).
        for year in 2000..=2027 {
            for month in 1..=12u8 {
                let days_in_month = utils::iso_days_in_month(year, month);
                let first_day_secs = utils::epoch_days_from_gregorian_date(year, month, 1) * 86_400;
                let first_dow = utils::epoch_seconds_to_day_of_week(first_day_secs);

                for rule_week in 1..=5u8 {
                    for rule_day in 0..=6u8 {
                        let first_occurrence = 1 + (rule_day + 7 - first_dow) % 7;
                        let mut transition_dom = first_occurrence + (rule_week - 1) * 7;
                        if transition_dom > days_in_month {
                            transition_dom -= 7;
                        }

                        for dom in 1..=days_in_month {
                            let secs = utils::epoch_days_from_gregorian_date(year, month, dom)
                                * 86_400
                                + 43_200;
                            let mwd = MwdForTime::from_seconds(secs);
                            let mut rule = Mwd::from_u8(month, rule_week, rule_day);
                            mwd.normalize_mwd(&mut rule);

                            assert_eq!(
                                mwd.mwd.cmp(&rule),
                                dom.cmp(&transition_dom),
                                "mismatch for {year}-{month:02}-{dom:02} vs M{month}.{rule_week}.{rule_day} (transition day {transition_dom})"
                            );
                        }
                    }
                }
            }
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
}
