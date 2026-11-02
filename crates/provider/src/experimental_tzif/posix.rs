use crate::{
    common::{
        calculate_transition_seconds_for_year, DstTransitionInfoForYear, LocalTimeRecordResult,
        PosixTransitionDay, TimeZoneTransitionInfo,
    },
    provider::{TimeZoneProviderResult, UtcOffsetSeconds},
    TimeZoneProviderError,
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

        DstTransitionInfoForYear::resolve_local_seconds(local_seconds, |year| {
            DstTransitionInfoForYear::compute_zero_transition(self.offset, transition_info, year)
        })
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

        DstTransitionInfoForYear::resolve_epoch_seconds(epoch_seconds, |year| {
            DstTransitionInfoForYear::compute_zero_transition(
                self.offset,
                dst_transition_info,
                year,
            )
        })
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
