//! Precipitation level classes and their mm/h ranges.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Highest valid precipitation level.
pub const MAX_LEVEL: u8 = 9;

/// Inclusive lower bounds (mm/h) for levels 0..=9.
///
/// Level 0 (no data) has no lower bound. Levels 1..=9 start at 0, 0.25, 1, 5,
/// 10, 20, 30, 50 and 80 mm/h respectively.
pub const LEVEL_MIN_MM_H: [Option<f64>; 10] = [
    None,
    Some(0.0),
    Some(0.25),
    Some(1.0),
    Some(5.0),
    Some(10.0),
    Some(20.0),
    Some(30.0),
    Some(50.0),
    Some(80.0),
];

/// Exclusive upper bounds (mm/h) for levels 0..=9.
///
/// Level 0 (no data) and level 9 (>= 80 mm/h) have no upper bound.
pub const LEVEL_MAX_MM_H: [Option<f64>; 10] = [
    None,
    Some(0.25),
    Some(1.0),
    Some(5.0),
    Some(10.0),
    Some(20.0),
    Some(30.0),
    Some(50.0),
    Some(80.0),
    None,
];

/// `(lower, upper)` bounds for each level, as a lower-inclusive and
/// upper-exclusive mm/h range.
pub const LEVEL_BOUNDS: [(Option<f64>, Option<f64>); 10] = [
    (None, None),
    (Some(0.0), Some(0.25)),
    (Some(0.25), Some(1.0)),
    (Some(1.0), Some(5.0)),
    (Some(5.0), Some(10.0)),
    (Some(10.0), Some(20.0)),
    (Some(20.0), Some(30.0)),
    (Some(30.0), Some(50.0)),
    (Some(50.0), Some(80.0)),
    (Some(80.0), None),
];

/// Semantic state of a precipitation class.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrecipitationStatus {
    /// No observation is available (level 0).
    NoData,
    /// An observation is available and reports no rain (level 1).
    NoRain,
    /// Rain is reported (levels 2..=9).
    Rain,
}

/// Alias for [`PrecipitationStatus`].
pub type Status = PrecipitationStatus;

impl PrecipitationStatus {
    /// Stable snake-case representation used in JSON.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoData => "no_data",
            Self::NoRain => "no_rain",
            Self::Rain => "rain",
        }
    }
}

impl fmt::Display for PrecipitationStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialEq<&str> for PrecipitationStatus {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<PrecipitationStatus> for &str {
    fn eq(&self, other: &PrecipitationStatus) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq<String> for PrecipitationStatus {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<PrecipitationStatus> for String {
    fn eq(&self, other: &PrecipitationStatus) -> bool {
        self == other.as_str()
    }
}

/// A precipitation class as a JSON-friendly range in mm/h.
///
/// The range is lower-inclusive and upper-exclusive. Missing bounds are
/// represented by `None` and serialise as JSON `null`.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Precipitation {
    /// Class index, 0..=9.
    pub level: u8,
    /// Whether this class means no data, no rain, or rain.
    pub status: PrecipitationStatus,
    /// Inclusive lower bound in mm/h, or `None` when unbounded.
    pub min_mm_h: Option<f64>,
    /// Exclusive upper bound in mm/h, or `None` when unbounded.
    pub max_mm_h: Option<f64>,
}

/// Errors returned when looking up a precipitation class.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum LevelError {
    /// The level is outside `0..=9`.
    InvalidLevel(u8),
}

impl fmt::Display for LevelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLevel(level) => {
                write!(f, "level must be between 0 and {MAX_LEVEL}, got {level}")
            }
        }
    }
}

impl std::error::Error for LevelError {}

/// Alias for [`LevelError`].
pub type Error = LevelError;

/// Return the precipitation class for `level`.
///
/// Level 0 is no data, level 1 is no rain (the `[0, 0.25)` mm/h class), and
/// levels 2..=9 are rain classes. Level 9 has no upper bound.
pub fn precipitation(level: u8) -> Result<Precipitation, LevelError> {
    if level > MAX_LEVEL {
        return Err(LevelError::InvalidLevel(level));
    }

    let status = match level {
        0 => PrecipitationStatus::NoData,
        1 => PrecipitationStatus::NoRain,
        _ => PrecipitationStatus::Rain,
    };

    Ok(Precipitation {
        level,
        status,
        min_mm_h: LEVEL_MIN_MM_H[level as usize],
        max_mm_h: LEVEL_MAX_MM_H[level as usize],
    })
}

impl TryFrom<u8> for Precipitation {
    type Error = LevelError;

    fn try_from(level: u8) -> Result<Self, Self::Error> {
        precipitation(level)
    }
}
