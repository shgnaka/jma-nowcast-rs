//! Timeline resolution and target time selection for JMA nowcast tiles (R4).

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fmt;

/// An entry in targetTimes_N1.json or targetTimes_N2.json.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetTimeEntry {
    #[serde(rename = "basetime")]
    pub basetime: String,
    #[serde(rename = "validtime")]
    pub validtime: String,
    #[serde(default, rename = "elements")]
    pub elements: Vec<String>,
}

/// Resolved timeline containing observation and forecasts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NowcastTimeline {
    pub basetime: DateTime<Utc>,
    pub observation_validtime: DateTime<Utc>,
    /// 13 forecast targets from basetime + 0m to +60m in 5-minute increments (or 0m..60m).
    pub forecast_validtimes: Vec<DateTime<Utc>>,
}

/// Errors occurring during timeline selection or validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeError {
    InvalidFormat(String),
    StaleData {
        basetime: DateTime<Utc>,
        now: DateTime<Utc>,
    },
    FutureBasetime {
        basetime: DateTime<Utc>,
        now: DateTime<Utc>,
    },
    MissingTimes(String),
    NoCompleteSeries,
}

impl fmt::Display for TimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFormat(s) => write!(f, "invalid timestamp format: {s}"),
            Self::StaleData { basetime, now } => {
                write!(
                    f,
                    "basetime {basetime} is older than 20 minutes (now: {now})"
                )
            }
            Self::FutureBasetime { basetime, now } => {
                write!(f, "basetime {basetime} is in the future (now: {now})")
            }
            Self::MissingTimes(msg) => write!(f, "missing target times: {msg}"),
            Self::NoCompleteSeries => write!(f, "no complete 0..60 minute nowcast series found"),
        }
    }
}

impl std::error::Error for TimeError {}

/// Parse JMA 14-digit UTC timestamp `YYYYMMDDHHMMSS` into `DateTime<Utc>`.
pub fn parse_jma_timestamp(s: &str) -> Result<DateTime<Utc>, TimeError> {
    let bytes = s.as_bytes();
    if bytes.len() != 14 || !bytes.iter().all(|b| b.is_ascii_digit()) {
        return Err(TimeError::InvalidFormat(s.to_string()));
    }

    let parse_num = |start: usize, end: usize| -> Result<u32, TimeError> {
        s[start..end]
            .parse::<u32>()
            .map_err(|_| TimeError::InvalidFormat(s.to_string()))
    };

    let year = parse_num(0, 4)? as i32;
    let month = parse_num(4, 6)?;
    let day = parse_num(6, 8)?;
    let hour = parse_num(8, 10)?;
    let min = parse_num(10, 12)?;
    let sec = parse_num(12, 14)?;

    let date = NaiveDate::from_ymd_opt(year, month, day)
        .ok_or_else(|| TimeError::InvalidFormat(s.to_string()))?;
    let dt = date
        .and_hms_opt(hour, min, sec)
        .ok_or_else(|| TimeError::InvalidFormat(s.to_string()))?;

    Ok(Utc.from_utc_datetime(&dt))
}

/// Format `DateTime<Utc>` into JMA 14-digit UTC timestamp `YYYYMMDDHHMMSS`.
pub fn format_jma_timestamp(dt: &DateTime<Utc>) -> String {
    format!(
        "{:04}{:02}{:02}{:02}{:02}{:02}",
        dt.year(),
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second()
    )
}

/// Format `DateTime<Utc>` into RFC3339 string with trailing Z.
pub fn format_rfc3339(dt: &DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Helper to check if entry has "hrpns" element
pub fn is_hrpns(entry: &TargetTimeEntry) -> bool {
    entry.elements.iter().any(|e| e == "hrpns")
}

/// Select latest valid N1 analysis basetime for lead 0 queries (validtime == basetime).
pub fn select_latest_n1_analysis(
    n1_entries: &[TargetTimeEntry],
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>, TimeError> {
    let mut candidates = Vec::new();
    for entry in n1_entries {
        if !is_hrpns(entry) {
            continue;
        }
        let bt = parse_jma_timestamp(&entry.basetime)?;
        let vt = parse_jma_timestamp(&entry.validtime)?;
        if bt == vt {
            candidates.push(bt);
        }
    }

    candidates.sort_unstable();
    candidates.dedup();

    for &bt in candidates.iter().rev() {
        if bt > now {
            continue;
        }
        if now.signed_duration_since(bt) > Duration::minutes(20) {
            return Err(TimeError::StaleData { basetime: bt, now });
        }
        return Ok(bt);
    }

    if let Some(&latest) = candidates.iter().next_back() {
        if latest > now {
            return Err(TimeError::FutureBasetime {
                basetime: latest,
                now,
            });
        }
    }

    Err(TimeError::MissingTimes(
        "no valid N1 analysis found".to_string(),
    ))
}

/// Select the latest complete nowcast timeline from N1 and N2 targetTimes lists.
///
/// Rules:
/// - Only N1 analysis (validtime == basetime) and N2 forecasts (validtime > basetime) tagged hrpns.
/// - Never use N2 0-minute entry or N1 nonzero entries.
/// - Group each complete set (0m from N1, 5..60m in 5m steps from N2) and fallback to older complete set if latest N1 lacks N2.
/// - basetime must not be older than 20 minutes from `now`.
/// - basetime must not be in the future relative to `now`.
pub fn select_nowcast_timeline_from_n1_n2(
    n1_entries: &[TargetTimeEntry],
    n2_entries: &[TargetTimeEntry],
    now: DateTime<Utc>,
) -> Result<NowcastTimeline, TimeError> {
    let mut n1_analyses: HashSet<DateTime<Utc>> = HashSet::new();
    let mut all_basetimes: BTreeMap<DateTime<Utc>, HashSet<DateTime<Utc>>> = BTreeMap::new();

    for entry in n1_entries {
        if !is_hrpns(entry) {
            continue;
        }
        let bt = parse_jma_timestamp(&entry.basetime)?;
        let vt = parse_jma_timestamp(&entry.validtime)?;
        if bt == vt {
            n1_analyses.insert(bt);
            all_basetimes.entry(bt).or_default().insert(vt);
        }
    }

    for entry in n2_entries {
        if !is_hrpns(entry) {
            continue;
        }
        let bt = parse_jma_timestamp(&entry.basetime)?;
        let vt = parse_jma_timestamp(&entry.validtime)?;
        if vt > bt {
            all_basetimes.entry(bt).or_default().insert(vt);
        }
    }

    // Check complete sets in descending basetime order
    for (&bt, validtimes) in all_basetimes.iter().rev() {
        // Must have N1 analysis
        if !n1_analyses.contains(&bt) {
            continue;
        }

        // Must not be future
        if bt > now {
            continue;
        }

        // Must not be stale (> 20 mins)
        if now.signed_duration_since(bt) > Duration::minutes(20) {
            return Err(TimeError::StaleData { basetime: bt, now });
        }

        // Check 0..60m in 5m steps
        let mut complete = true;
        let mut expected = Vec::with_capacity(13);
        for step in 0..=12 {
            let vt = bt + Duration::minutes(step * 5);
            expected.push(vt);
            if !validtimes.contains(&vt) {
                complete = false;
                break;
            }
        }

        if complete {
            return Ok(NowcastTimeline {
                basetime: bt,
                observation_validtime: bt,
                forecast_validtimes: expected,
            });
        }
    }

    // If latest basetime overall is in the future
    if let Some(&latest_bt) = all_basetimes.keys().next_back() {
        if latest_bt > now {
            return Err(TimeError::FutureBasetime {
                basetime: latest_bt,
                now,
            });
        }
    }

    Err(TimeError::NoCompleteSeries)
}

/// Legacy / backward compatible wrapper around select_nowcast_timeline_from_n1_n2.
pub fn select_nowcast_timeline(
    entries: &[TargetTimeEntry],
    now: DateTime<Utc>,
) -> Result<NowcastTimeline, TimeError> {
    // Treat entries where validtime == basetime as N1 and validtime > basetime as N2
    let mut n1 = Vec::new();
    let mut n2 = Vec::new();
    for entry in entries {
        if let (Ok(bt), Ok(vt)) = (
            parse_jma_timestamp(&entry.basetime),
            parse_jma_timestamp(&entry.validtime),
        ) {
            if vt == bt {
                n1.push(entry.clone());
            } else if vt > bt {
                n2.push(entry.clone());
            }
        }
    }
    select_nowcast_timeline_from_n1_n2(&n1, &n2, now)
}
