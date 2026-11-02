use crate::{
    common::{
        calculate_transition_seconds_for_year, offset_range, resolve_posix_epoch_seconds,
        DstTransitionInfoForYear, LocalTimeRecordResult, Mwd, MwdForTime, PosixTransitionDay,
        TimeZoneTransitionInfo, TransitionType,
    },
    epoch_nanoseconds::EpochNanoseconds,
    provider::{GapEntryOffsets, TimeZoneProviderResult, UtcOffsetSeconds},
    utils, TimeZoneProviderError,
};

use tinystr::TinyAsciiStr;
#[cfg(feature = "datagen")]
use zoneinfo_rs::posix::{MonthWeekDay, PosixDate, PosixDateTime, PosixTimeZone, PosixTransition};

#[zerovec::make_ule(PosixZoneULE)]
#[derive(PartialEq, Eq, Debug, Clone, Copy, PartialOrd, Ord)]
#[cfg_attr(
    feature = "datagen",
    derive(yoke::Yokeable, serde::Serialize, databake::Bake)
)]
#[cfg_attr(feature = "datagen", databake(path = timezone_provider::experimental_tzif::posix))]
pub struct PosixZone {
    pub abbr: TinyAsciiStr<5>,
    pub offset: i64,
    pub transition: Option<ZeroPosixTransition>,
}

impl PosixZone {
    pub(crate) fn resolve_for_local_seconds(
        &self,
        local_seconds: i64,
    ) -> TimeZoneProviderResult<LocalTimeRecordResult> {
        let Some(transition_info) = &self.transition else {
            // Regardless of the time, there is one variant and we can return it.
            let offset = UtcOffsetSeconds(self.offset);
            return Ok(LocalTimeRecordResult::Single(offset));
        };

        // NOTE:
        // STD -> DST == start
        // DST -> STD == end
        let (is_transition_day, mut is_dst) = cmp_seconds_to_transitions(
            &transition_info.start.date,
            &transition_info.end.date,
            local_seconds,
        )?;

        if is_transition_day {
            let time = utils::epoch_ms_to_ms_in_day(local_seconds * 1_000) as i64 / 1_000;
            let transition_time = if is_dst == TransitionType::Dst {
                transition_info.start.time
            } else {
                transition_info.end.time
            };
            // Convert to UtcOffsetSeconds so that these behave like
            // normal offsets
            let std = UtcOffsetSeconds(self.offset);
            let dst = UtcOffsetSeconds(self.offset + transition_info.savings);
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
            TransitionType::Dst => {
                Ok(UtcOffsetSeconds(self.offset + transition_info.savings).into())
            }
            TransitionType::Std => Ok(UtcOffsetSeconds(self.offset).into()),
        }
    }

    pub(crate) fn resolve_for_epoch_seconds(
        &self,
        epoch_seconds: i64,
    ) -> TimeZoneProviderResult<TimeZoneTransitionInfo> {
        let Some(dst_transition_info) = &self.transition else {
            // Regardless of the time, there is one variant and we can return it.
            return Ok(TimeZoneTransitionInfo {
                transition_epoch: None,
                offset: UtcOffsetSeconds(self.offset),
            });
        };

        Ok(resolve_posix_epoch_seconds(
            (
                dst_transition_info.start.date.to_posix_day()?,
                dst_transition_info.start.time,
            ),
            (
                dst_transition_info.end.date.to_posix_day()?,
                dst_transition_info.end.time,
            ),
            UtcOffsetSeconds(self.offset),
            UtcOffsetSeconds(self.offset + dst_transition_info.savings),
            epoch_seconds,
        ))
    }
}

impl DstTransitionInfoForYear {
    pub(crate) fn compute_zero_transition(
        std_offset_seconds: i64,
        dst_transition: &ZeroPosixTransition,
        year: i32,
    ) -> TimeZoneProviderResult<Self> {
        let std_offset = UtcOffsetSeconds(std_offset_seconds);
        let dst_offset = UtcOffsetSeconds(std_offset_seconds + dst_transition.savings);
        let dst_start_seconds = calculate_transition_seconds_for_year(
            year,
            dst_transition.start.date.to_posix_day()?,
            dst_transition.start.time,
            std_offset,
        );
        let dst_end_seconds = calculate_transition_seconds_for_year(
            year,
            dst_transition.end.date.to_posix_day()?,
            dst_transition.end.time,
            dst_offset,
        );
        Ok(Self {
            dst_start_seconds,
            dst_end_seconds,
            std_offset,
            dst_offset,
        })
    }
}

fn cmp_seconds_to_transitions(
    start: &ZeroTransitionDate,
    end: &ZeroTransitionDate,
    seconds: i64,
) -> TimeZoneProviderResult<(bool, TransitionType)> {
    let cmp_result = match (start.to_posix_day()?, end.to_posix_day()?) {
        (
            PosixTransitionDay::Mwd(start_month, start_week, start_day),
            PosixTransitionDay::Mwd(end_month, end_week, end_day),
        ) => {
            let mwd = MwdForTime::from_seconds(seconds);
            let mut start = Mwd::from_u8(start_month as u8, start_week as u8, start_day as u8);
            let mut end = Mwd::from_u8(end_month as u8, end_week as u8, end_day as u8);

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

#[cfg(feature = "datagen")]
#[allow(clippy::unwrap_used, reason = "Datagen only")]
impl From<&PosixTimeZone> for PosixZone {
    fn from(value: &PosixTimeZone) -> Self {
        let abbr = TinyAsciiStr::<5>::try_from_str(&value.abbr.formatted).unwrap();
        let offset = value.offset.as_secs();
        let transition = value
            .transition_info
            .as_ref()
            .map(ZeroPosixTransition::from);

        Self {
            abbr,
            offset,
            transition,
        }
    }
}

#[zerovec::make_ule(PosixTransitionULE)]
#[derive(PartialEq, Eq, Debug, Clone, Copy, PartialOrd, Ord)]
#[cfg_attr(
    feature = "datagen",
    derive(yoke::Yokeable, serde::Serialize, databake::Bake)
)]
#[cfg_attr(feature = "datagen", databake(path = timezone_provider::experimental_tzif::posix))]
pub struct ZeroPosixTransition {
    pub abbr: TinyAsciiStr<5>,
    pub savings: i64,
    pub start: ZeroTransitionDateTime,
    pub end: ZeroTransitionDateTime,
}

#[cfg(feature = "datagen")]
#[allow(clippy::unwrap_used, reason = "Datagen only")]
impl From<&PosixTransition> for ZeroPosixTransition {
    fn from(value: &PosixTransition) -> Self {
        let abbr = TinyAsciiStr::<5>::try_from_str(&value.abbr.formatted).unwrap();
        let savings = value.savings.as_secs();
        let start = ZeroTransitionDateTime::from(&value.start);
        let end = ZeroTransitionDateTime::from(&value.end);
        Self {
            abbr,
            savings,
            start,
            end,
        }
    }
}

#[zerovec::make_ule(TransitionDateTimeULE)]
#[derive(PartialEq, Eq, Debug, Clone, Copy, PartialOrd, Ord)]
#[cfg_attr(
    feature = "datagen",
    derive(yoke::Yokeable, serde::Serialize, databake::Bake)
)]
#[cfg_attr(feature = "datagen", databake(path = timezone_provider::experimental_tzif::posix))]
pub struct ZeroTransitionDateTime {
    /// The date at which a transition should occur.
    pub date: ZeroTransitionDate,
    /// The time of day in seconds.
    pub time: i64,
}

#[cfg(feature = "datagen")]
impl From<&PosixDateTime> for ZeroTransitionDateTime {
    fn from(value: &PosixDateTime) -> Self {
        Self {
            date: value.date.into(),
            time: value.time.as_secs(),
        }
    }
}

#[zerovec::make_ule(DateULE)]
#[derive(PartialEq, Eq, Debug, Clone, Copy, PartialOrd, Ord)]
#[cfg_attr(
    feature = "datagen",
    derive(yoke::Yokeable, serde::Serialize, databake::Bake)
)]
#[cfg_attr(feature = "datagen", databake(path = timezone_provider::experimental_tzif::posix))]
pub struct ZeroTransitionDate {
    pub kind: DateKind,
    pub day: Option<u16>,
    pub mwd: Option<(u8, u8, u8)>,
}

impl ZeroTransitionDate {
    pub(crate) fn to_posix_day(self) -> TimeZoneProviderResult<PosixTransitionDay> {
        match self {
            ZeroTransitionDate {
                kind: DateKind::JulianNoLeap,
                day: Some(day),
                ..
            } => Ok(PosixTransitionDay::JulianNoLeap(day)),
            ZeroTransitionDate {
                kind: DateKind::Julian,
                day: Some(day),
                ..
            } => Ok(PosixTransitionDay::JulianWithLeap(day)),
            ZeroTransitionDate {
                kind: DateKind::MonthWeekDay,
                mwd: Some((month, week, day)),
                ..
            } => Ok(PosixTransitionDay::Mwd(
                u16::from(month),
                u16::from(week),
                u16::from(day),
            )),
            _ => Err(TimeZoneProviderError::Assert("Invalid ZeroTransitionDate")),
        }
    }
}

#[cfg(feature = "datagen")]
impl From<PosixDate> for ZeroTransitionDate {
    fn from(value: PosixDate) -> Self {
        let (kind, day, mwd) = match value {
            PosixDate::JulianLeap(day) => (DateKind::Julian, Some(day), None),
            PosixDate::JulianNoLeap(day) => (DateKind::JulianNoLeap, Some(day), None),
            PosixDate::MonthWeekDay(MonthWeekDay(month, week, day)) => (
                DateKind::MonthWeekDay,
                None,
                Some((month as u8, week, day as u8)),
            ),
        };
        Self { kind, day, mwd }
    }
}
#[zerovec::make_ule(DateKindULE)]
#[derive(PartialEq, Eq, Debug, Clone, Copy, PartialOrd, Ord)]
#[cfg_attr(
    feature = "datagen",
    derive(yoke::Yokeable, serde::Serialize, databake::Bake)
)]
#[cfg_attr(feature = "datagen", databake(path = timezone_provider::experimental_tzif::posix))]
#[repr(u8)]
pub enum DateKind {
    JulianNoLeap = 0,
    Julian = 1,
    MonthWeekDay = 2,
}
