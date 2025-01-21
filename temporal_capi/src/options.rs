#[diplomat::bridge]
#[diplomat::abi_rename = "temporal_rs_{0}"]
#[diplomat::attr(auto, namespace = "temporal_rs")]
pub mod ffi {
    use diplomat_runtime::DiplomatOption;
    use temporal_rs::options;

    #[diplomat::enum_convert(options::ArithmeticOverflow)]
    pub enum ArithmeticOverflow {
        Constrain,
        Reject,
    }
    #[diplomat::enum_convert(options::Disambiguation)]
    pub enum Disambiguation {
        Compatible,
        Earlier,
        Later,
        Reject,
    }

    #[diplomat::enum_convert(options::DisplayCalendar)]
    pub enum DisplayCalendar {
        Auto,
        Always,
        Never,
        Critical,
    }

    #[diplomat::enum_convert(options::DisplayOffset)]
    pub enum DisplayOffset {
        Auto,
        Never,
    }

    #[diplomat::enum_convert(options::DisplayTimeZone)]
    pub enum DisplayTimeZone {
        Auto,
        Never,
        Critical,
    }

    #[diplomat::enum_convert(options::DurationOverflow)]
    pub enum DurationOverflow {
        Constrain,
        Balance,
    }

    #[diplomat::enum_convert(options::OffsetDisambiguation)]
    pub enum OffsetDisambiguation {
        Use,
        Prefer,
        Ignore,
        Reject,
    }

    #[diplomat::enum_convert(options::TemporalRoundingMode)]
    pub enum TemporalRoundingMode {
        Ceil,
        Floor,
        Expand,
        Trunc,
        HalfCeil,
        HalfFloor,
        HalfExpand,
        HalfTrunc,
        HalfEven,
    }

    #[diplomat::enum_convert(options::TemporalUnit)]
    pub enum TemporalUnit {
        Auto = 0,
        Nanosecond = 1,
        Microsecond = 2,
        Millisecond = 3,
        Second = 4,
        Minute = 5,
        Hour = 6,
        Day = 7,
        Week = 8,
        Month = 9,
        Year = 10,
    }

    #[diplomat::enum_convert(options::TemporalUnsignedRoundingMode)]
    pub enum TemporalUnsignedRoundingMode {
        Infinity,
        Zero,
        HalfInfinity,
        HalfZero,
        HalfEven,
    }

    pub struct Precision {
        /// Sets the precision to minute precision.
        pub is_minute: bool,
        /// Sets the number of digits. Auto when None. Has no effect if is_minute is set.
        pub precision: DiplomatOption<u8>,
    }

    pub struct ToStringRoundingOptions {
        pub precision: Precision,
        pub smallest_unit: DiplomatOption<TemporalUnit>,
        pub rounding_mode: DiplomatOption<TemporalRoundingMode>,
    }
}

impl From<ffi::Precision> for temporal_rs::parsers::Precision {
    fn from(other: ffi::Precision) -> Self {
        if other.is_minute {
            Self::Minute
        } else if let Some(digit) = other.precision.into() {
            Self::Digit(digit)
        } else {
            Self::Auto
        }
    }
}

impl From<ffi::ToStringRoundingOptions> for temporal_rs::options::ToStringRoundingOptions {
    fn from(other: ffi::ToStringRoundingOptions) -> Self {
        Self {
            precision: other.precision.into(),
            smallest_unit: other.smallest_unit.into_converted_option(),
            rounding_mode: other.rounding_mode.into_converted_option(),
        }
    }
}
