//! An implementation of the Temporal Instant.

use std::num::NonZeroU128;

use crate::{
    components::{duration::TimeDuration, Duration},
    options::{
        DifferenceOperation, DifferenceSettings, ResolvedRoundingOptions, RoundingIncrement,
        TemporalRoundingMode, TemporalUnit,
    },
    rounding::{IncrementRounder, Round},
    TemporalError, TemporalResult, TemporalUnwrap, MS_PER_DAY, NS_PER_DAY,
};

use num_bigint::BigInt;
use num_traits::{FromPrimitive, ToPrimitive};

const NANOSECONDS_PER_SECOND: f64 = 1e9;
const NANOSECONDS_PER_MINUTE: f64 = 60f64 * NANOSECONDS_PER_SECOND;
const NANOSECONDS_PER_HOUR: f64 = 60f64 * NANOSECONDS_PER_MINUTE;

/// The native Rust implementation of `Temporal.Instant`
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Instant {
    pub(crate) nanos: BigInt,
}

// ==== Private API ====

impl Instant {
    /// Adds a `TimeDuration` to the current `Instant`.
    ///
    /// Temporal-Proposal equivalent: `AddDurationToOrSubtractDurationFrom`.
    pub(crate) fn add_to_instant(&self, duration: &TimeDuration) -> TemporalResult<Self> {
        let result = self.epoch_nanoseconds()
            + duration.nanoseconds
            + (duration.microseconds * 1000f64)
            + (duration.milliseconds * 1_000_000f64)
            + (duration.seconds * NANOSECONDS_PER_SECOND)
            + (duration.minutes * NANOSECONDS_PER_MINUTE)
            + (duration.hours * NANOSECONDS_PER_HOUR);
        let nanos = BigInt::from_f64(result).ok_or_else(|| {
            TemporalError::range().with_message("Duration added to instant exceeded valid range.")
        })?;
        Self::new(nanos)
    }

    // TODO: Add test for `diff_instant`.
    // NOTE(nekevss): As the below is internal, op will be left as a boolean
    // with a `since` op being true and `until` being false.
    /// Internal operation to handle `since` and `until` difference ops.
    #[allow(unused)]
    pub(crate) fn diff_instant(
        &self,
        op: DifferenceOperation,
        other: &Self,
        options: DifferenceSettings,
    ) -> TemporalResult<TimeDuration> {
        // diff the instant and determine its component values.
        let diff = self.to_f64() - other.to_f64();
        let nanos = diff.rem_euclid(1000f64);
        let micros = (diff / 1000f64).trunc().rem_euclid(1000f64);
        let millis = (diff / 1_000_000f64).trunc().rem_euclid(1000f64);
        let secs = (diff / NANOSECONDS_PER_SECOND).trunc();

        // Handle the settings provided to `diff_instant`
        let (sign, resolved) = ResolvedRoundingOptions::from_diff_settings(
            options,
            op,
            TemporalUnit::Second,
            TemporalUnit::Nanosecond,
        )?;

        // TODO: validate roundingincrement
        // Steps 11-13 of 13.47 GetDifferenceSettings

        if resolved.smallest_unit == TemporalUnit::Nanosecond {
            let (_, result) = TimeDuration::from_normalized(
                TimeDuration::new_unchecked(0f64, 0f64, secs, millis, micros, nanos)
                    .to_normalized(),
                resolved.largest_unit,
            )?;
            return Ok(result);
        }

        let normalized_time_duration =
            TimeDuration::new(0f64, 0f64, secs, millis, micros, nanos)?.to_normalized();

        let (round_result, _) = TimeDuration::round(0.0, &normalized_time_duration, resolved)?;
        let (_, result) = TimeDuration::from_normalized(
            round_result.normalized_time_duration(),
            resolved.largest_unit,
        )?;
        Ok(result)
    }

    /// Rounds a current `Instant` given the resolved options, returning a `BigInt` result.
    pub(crate) fn round_instant(
        &self,
        increment: RoundingIncrement,
        unit: TemporalUnit,
        rounding_mode: TemporalRoundingMode,
    ) -> TemporalResult<BigInt> {
        let increment = increment.as_extended_increment();
        let increment = match unit {
            TemporalUnit::Hour => increment
                .checked_mul(NonZeroU128::new(NANOSECONDS_PER_HOUR as u128).temporal_unwrap()?),
            TemporalUnit::Minute => increment
                .checked_mul(NonZeroU128::new(NANOSECONDS_PER_MINUTE as u128).temporal_unwrap()?),
            TemporalUnit::Second => increment
                .checked_mul(NonZeroU128::new(NANOSECONDS_PER_SECOND as u128).temporal_unwrap()?),
            TemporalUnit::Millisecond => {
                increment.checked_mul(NonZeroU128::new(1_000_000).temporal_unwrap()?)
            }
            TemporalUnit::Microsecond => {
                increment.checked_mul(NonZeroU128::new(1_000).temporal_unwrap()?)
            }
            TemporalUnit::Nanosecond => Some(increment),
            _ => {
                return Err(TemporalError::range()
                    .with_message("Invalid unit provided for Instant::round."))
            }
        };

        // NOTE: Potentially remove the below and just `temporal_unwrap`
        let Some(increment) = increment else {
            return Err(TemporalError::range().with_message("Increment exceeded a valid range."));
        };

        let rounded = IncrementRounder::<f64>::from_positive_parts(self.to_f64(), increment)?
            .round_as_positive(rounding_mode);

        BigInt::from_u64(rounded)
            .ok_or_else(|| TemporalError::range().with_message("Invalid rounded Instant value."))
    }

    /// Utility for converting `Instant` to f64.
    ///
    /// # Panics
    ///
    /// This function will panic if called on an invalid `Instant`.
    pub(crate) fn to_f64(&self) -> f64 {
        self.nanos
            .to_f64()
            .expect("A valid instant is representable by f64.")
    }
}

// ==== Public API ====

impl Instant {
    /// Create a new validated `Instant`.
    #[inline]
    pub fn new(nanos: BigInt) -> TemporalResult<Self> {
        if !is_valid_epoch_nanos(&nanos) {
            return Err(TemporalError::range()
                .with_message("Instant nanoseconds are not within a valid epoch range."));
        }
        Ok(Self { nanos })
    }

    /// Adds a `Duration` to the current `Instant`, returning an error if the `Duration`
    /// contains a `DateDuration`.
    #[inline]
    pub fn add(&self, duration: Duration) -> TemporalResult<Self> {
        if !duration.is_time_duration() {
            return Err(TemporalError::range()
                .with_message("DateDuration values cannot be added to instant."));
        }
        self.add_time_duration(duration.time())
    }

    /// Adds a `TimeDuration` to `Instant`.
    #[inline]
    pub fn add_time_duration(&self, duration: &TimeDuration) -> TemporalResult<Self> {
        self.add_to_instant(duration)
    }

    /// Subtract a `Duration` to the current `Instant`, returning an error if the `Duration`
    /// contains a `DateDuration`.
    #[inline]
    pub fn subtract(&self, duration: Duration) -> TemporalResult<Self> {
        if !duration.is_time_duration() {
            return Err(TemporalError::range()
                .with_message("DateDuration values cannot be added to instant."));
        }
        self.subtract_time_duration(duration.time())
    }

    /// Subtracts a `TimeDuration` to `Instant`.
    #[inline]
    pub fn subtract_time_duration(&self, duration: &TimeDuration) -> TemporalResult<Self> {
        self.add_to_instant(&duration.negated())
    }

    /// Returns a `TimeDuration` representing the duration since provided `Instant`
    #[inline]
    pub fn since(
        &self,
        other: &Self,
        settings: DifferenceSettings,
    ) -> TemporalResult<TimeDuration> {
        self.diff_instant(DifferenceOperation::Since, other, settings)
    }

    /// Returns a `TimeDuration` representing the duration until provided `Instant`
    #[inline]
    pub fn until(
        &self,
        other: &Self,
        settings: DifferenceSettings,
    ) -> TemporalResult<TimeDuration> {
        self.diff_instant(DifferenceOperation::Until, other, settings)
    }

    /// Returns an `Instant` by rounding the current `Instant` according to the provided settings.
    pub fn round(
        &self,
        increment: Option<f64>,
        unit: TemporalUnit, // smallestUnit is required on Instant::round
        rounding_mode: Option<TemporalRoundingMode>,
    ) -> TemporalResult<Self> {
        let increment = RoundingIncrement::try_from(increment.unwrap_or(1.0))?;
        let mode = rounding_mode.unwrap_or(TemporalRoundingMode::HalfExpand);
        let maximum = match unit {
            TemporalUnit::Hour => 24u64,
            TemporalUnit::Minute => 24 * 60,
            TemporalUnit::Second => 24 * 3600,
            TemporalUnit::Millisecond => MS_PER_DAY as u64,
            TemporalUnit::Microsecond => MS_PER_DAY as u64 * 1000,
            TemporalUnit::Nanosecond => NS_PER_DAY,
            _ => return Err(TemporalError::range().with_message("Invalid roundTo unit provided.")),
        };
        // NOTE: to_rounding_increment returns an f64 within a u32 range.
        increment.validate(maximum, true)?;

        let round_result = self.round_instant(increment, unit, mode)?;
        Self::new(round_result)
    }

    /// Returns the `epochSeconds` value for this `Instant`.
    #[must_use]
    pub fn epoch_seconds(&self) -> f64 {
        (&self.nanos / BigInt::from(1_000_000_000))
            .to_f64()
            .expect("A validated Instant should be within a valid f64")
            .floor()
    }

    /// Returns the `epochMilliseconds` value for this `Instant`.
    #[must_use]
    pub fn epoch_milliseconds(&self) -> f64 {
        (&self.nanos / BigInt::from(1_000_000))
            .to_f64()
            .expect("A validated Instant should be within a valid f64")
            .floor()
    }

    /// Returns the `epochMicroseconds` value for this `Instant`.
    #[must_use]
    pub fn epoch_microseconds(&self) -> f64 {
        (&self.nanos / BigInt::from(1_000))
            .to_f64()
            .expect("A validated Instant should be within a valid f64")
            .floor()
    }

    /// Returns the `epochNanoseconds` value for this `Instant`.
    #[must_use]
    pub fn epoch_nanoseconds(&self) -> f64 {
        self.to_f64()
    }
}

// ==== Utility Functions ====

/// Utility for determining if the nanos are within a valid range.
#[inline]
#[must_use]
pub(crate) fn is_valid_epoch_nanos(nanos: &BigInt) -> bool {
    nanos <= &BigInt::from(crate::NS_MAX_INSTANT) && nanos >= &BigInt::from(crate::NS_MIN_INSTANT)
}

// ==== Instant Tests ====

#[cfg(test)]
mod tests {
    use crate::{components::Instant, NS_MAX_INSTANT, NS_MIN_INSTANT};
    use num_bigint::BigInt;
    use num_traits::ToPrimitive;

    #[test]
    #[allow(clippy::float_cmp)]
    fn max_and_minimum_instant_bounds() {
        // This test is primarily to assert that the `expect` in the epoch methods is
        // valid, i.e., a valid instant is within the range of an f64.
        let max = BigInt::from(NS_MAX_INSTANT);
        let min = BigInt::from(NS_MIN_INSTANT);
        let max_instant = Instant::new(max.clone()).unwrap();
        let min_instant = Instant::new(min.clone()).unwrap();

        assert_eq!(max_instant.epoch_nanoseconds(), max.to_f64().unwrap());
        assert_eq!(min_instant.epoch_nanoseconds(), min.to_f64().unwrap());

        let max_plus_one = BigInt::from(NS_MAX_INSTANT + 1);
        let min_minus_one = BigInt::from(NS_MIN_INSTANT - 1);

        assert!(Instant::new(max_plus_one).is_err());
        assert!(Instant::new(min_minus_one).is_err());
    }
}
