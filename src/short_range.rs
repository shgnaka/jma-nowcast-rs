//! JMA hourly short-range precipitation forecast (`rasrf`, R10).
//!
//! The `immed` member publishes one-hour accumulations for +1 h ... +6 h and
//! the `none` member for +7 h ... +15 h.  Both series are selected and fetched
//! independently from `targetTimes.json`.

use crate::client::{fetch_bytes_with_retry, ClientError, MAX_JSON_BODY_SIZE};
use crate::decode::decode_tile;
use crate::levels::LEVEL_BOUNDS;
use crate::tiles::{locate, Pixel, TILE_SIZE};
use crate::times::{format_jma_timestamp, format_rfc3339, parse_jma_timestamp};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::ops::RangeInclusive;

/// Published tile base for the JMA hourly short-range precipitation product.
pub const DEFAULT_SHORT_RANGE_BASE_URL: &str = "https://www.jma.go.jp/bosai/jmatile/data/rasrf";

/// Human-facing source page for the product.
pub const SOURCE_URL: &str = "https://www.jma.go.jp/bosai/kaikotan/";
/// Attribution text for the source data.
pub const SOURCE: &str = "Japan Meteorological Agency (JMA)";
/// Licence of the source data.
pub const LICENSE: &str = "PDL-1.0";

const ELEMENT: &str = "rasrf";
const IMMED_MEMBER: &str = "immed";
const EXTENDED_MEMBER: &str = "none";
const IMMED_MAX_AGE_MINUTES: i64 = 40;
const EXTENDED_MAX_AGE_MINUTES: i64 = 150;
const MAX_TILE_BYTES: usize = 4 * 1024 * 1024;

/// Semantic state of one raster class.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShortRangeStatus {
    /// Indexed palette index 0: no observation/data.
    NoData,
    /// Indexed palette index 1: below the display threshold.
    BelowDisplayThreshold,
    /// Non-indexed pixel 0: missing data and below-threshold rain are ambiguous.
    UnavailableOrBelowDisplayThreshold,
    /// A coloured precipitation class (levels 2..=9).
    Rain,
}

/// One forecast frame (a one-hour accumulation ending at `validtime`).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ShortRangeSample {
    /// Lead time in whole hours from the series basetime.
    pub lead_hours: u8,
    /// End of the accumulation as RFC3339 UTC.
    pub validtime: String,
    /// Start of the one-hour accumulation as RFC3339 UTC.
    pub accumulation_start: String,
    /// End of the one-hour accumulation as RFC3339 UTC.
    pub accumulation_end: String,
    /// Raster class, 0..=9.
    pub level: u8,
    /// Semantic state of the class.
    pub status: ShortRangeStatus,
    /// Inclusive lower bound in mm, or `None`.
    pub min_mm: Option<f64>,
    /// Exclusive upper bound in mm, or `None`.
    pub max_mm: Option<f64>,
}

/// One of the two independent forecast series.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ShortRangeSeries {
    /// Basetime as RFC3339 UTC.
    pub basetime: String,
    /// Member advertised by `targetTimes.json` and used in tile URLs.
    pub member: String,
    /// Forecast frames in ascending validtime order.
    pub samples: Vec<ShortRangeSample>,
}

/// Result of a point lookup in the hourly short-range forecast.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ShortRange {
    pub lat: f64,
    pub lon: f64,
    pub zoom: u8,
    /// Unit used by every accumulation bound. Always `"mm"`.
    pub unit: &'static str,
    /// The +1 h ... +6 h series (`immed`).
    pub immed: ShortRangeSeries,
    /// The +7 h ... +15 h series (`none`).
    pub none: ShortRangeSeries,
    /// All 15 frames in lead-time order.
    pub samples: Vec<ShortRangeSample>,
    pub source: &'static str,
    pub source_url: &'static str,
    pub processed: bool,
    pub processing: Vec<&'static str>,
    pub license: &'static str,
}

/// Errors returned by [`get_short_range`], using the library's stable codes.
#[derive(Debug)]
pub enum ShortRangeError {
    InvalidInput(String),
    StaleData(String),
    MissingTimes(String),
    UpstreamNotFound(String),
    UpstreamHttp(String),
    UpstreamData(String),
    Network(String),
}

impl ShortRangeError {
    /// Stable machine-readable error code.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidInput(_) => "invalid_input",
            Self::StaleData(_) => "stale_data",
            Self::MissingTimes(_) => "missing_times",
            Self::UpstreamNotFound(_) => "upstream_not_found",
            Self::UpstreamHttp(_) => "upstream_http",
            Self::UpstreamData(_) => "upstream_data",
            Self::Network(_) => "network",
        }
    }
}

impl fmt::Display for ShortRangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(s) => write!(f, "invalid input: {s}"),
            Self::StaleData(s) => write!(f, "stale data: {s}"),
            Self::MissingTimes(s) => write!(f, "missing times: {s}"),
            Self::UpstreamNotFound(s) => write!(f, "upstream not found: {s}"),
            Self::UpstreamHttp(s) => write!(f, "upstream HTTP error: {s}"),
            Self::UpstreamData(s) => write!(f, "upstream data error: {s}"),
            Self::Network(s) => write!(f, "network error: {s}"),
        }
    }
}

impl std::error::Error for ShortRangeError {}

impl From<ClientError> for ShortRangeError {
    fn from(value: ClientError) -> Self {
        match value {
            ClientError::InvalidInput(s) => Self::InvalidInput(s),
            ClientError::UpstreamNotFound(s) => Self::UpstreamNotFound(s),
            ClientError::UpstreamHttp(s) => Self::UpstreamHttp(s),
            ClientError::UpstreamData(s) => Self::UpstreamData(s),
            ClientError::Network(s) => Self::Network(s),
        }
    }
}

impl From<crate::decode::DecodeError> for ShortRangeError {
    fn from(value: crate::decode::DecodeError) -> Self {
        Self::UpstreamData(value.to_string())
    }
}

#[derive(Debug, Deserialize)]
struct TargetTimeEntry {
    #[serde(default)]
    basetime: String,
    #[serde(default)]
    validtime: String,
    #[serde(default)]
    member: Option<String>,
    #[serde(default)]
    elements: Vec<String>,
}

#[derive(Debug)]
struct Selection {
    member: &'static str,
    basetime: DateTime<Utc>,
    validtimes: Vec<(u8, DateTime<Utc>)>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Encoding {
    Indexed,
    Rgb,
    Rgba,
}

/// Fetch the hourly short-range forecast for one point.
///
/// The latest complete, non-future `immed` +1 h ... +6 h series and the latest
/// complete, non-future `none` +7 h ... +15 h series are selected independently.
/// One tile per frame is fetched and sampled at `(lat, lon)`.
pub async fn get_short_range(
    client: &reqwest::Client,
    base_url: &str,
    now: DateTime<Utc>,
    lat: f64,
    lon: f64,
    zoom: u8,
) -> Result<ShortRange, ShortRangeError> {
    let base_url = validate_base_url(base_url)?;
    let pixel =
        locate(lat, lon, zoom).map_err(|error| ShortRangeError::InvalidInput(error.to_string()))?;

    let target_times_url = format!("{base_url}/targetTimes.json");
    let body = fetch_bytes_with_retry(client, &target_times_url, MAX_JSON_BODY_SIZE).await?;
    let entries: Vec<TargetTimeEntry> = serde_json::from_slice(&body)
        .map_err(|error| ShortRangeError::UpstreamData(format!("targetTimes.json: {error}")))?;

    let immed_selection = match select_series(
        &entries,
        IMMED_MEMBER,
        1..=6,
        IMMED_MAX_AGE_MINUTES,
        now,
        None,
    ) {
        Ok(selection) => selection,
        Err(error)
            if matches!(
                error,
                ShortRangeError::MissingTimes(_) | ShortRangeError::StaleData(_)
            ) =>
        {
            let original_error = error;
            match select_series(
                &entries,
                EXTENDED_MEMBER,
                1..=6,
                IMMED_MAX_AGE_MINUTES,
                now,
                None,
            ) {
                Ok(selection) => selection,
                Err(ShortRangeError::MissingTimes(_)) => return Err(original_error),
                Err(other) => return Err(other),
            }
        }
        Err(error) => return Err(error),
    };
    let none_cutoff = immed_selection
        .validtimes
        .last()
        .map(|&(_, validtime)| validtime);
    let none_selection = select_series(
        &entries,
        EXTENDED_MEMBER,
        7..=15,
        EXTENDED_MAX_AGE_MINUTES,
        now,
        none_cutoff,
    )?;

    let immed = fetch_series(client, &base_url, &pixel, &immed_selection).await?;
    let none = fetch_series(client, &base_url, &pixel, &none_selection).await?;
    let samples = immed
        .samples
        .iter()
        .chain(none.samples.iter())
        .cloned()
        .collect();

    Ok(ShortRange {
        lat,
        lon,
        zoom,
        unit: "mm",
        immed,
        none,
        samples,
        source: SOURCE,
        source_url: SOURCE_URL,
        processed: true,
        processing: vec![
            "selected the latest complete non-future immed (+1h..+6h) and none (+7h..+15h) series",
            "sampled the point tile for every hourly accumulation frame",
            "interpreted each raster class as a one-hour accumulation in mm",
        ],
        license: LICENSE,
    })
}

fn validate_base_url(base_url: &str) -> Result<String, ShortRangeError> {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.is_empty() || !(trimmed.starts_with("https://") || trimmed.starts_with("http://")) {
        return Err(ShortRangeError::InvalidInput(
            "base_url must be an http:// or https:// URL".to_string(),
        ));
    }
    if trimmed.contains('?') || trimmed.contains('#') || trimmed.chars().any(char::is_whitespace) {
        return Err(ShortRangeError::InvalidInput(
            "base_url must not contain whitespace, a query string or a fragment".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

fn select_series(
    entries: &[TargetTimeEntry],
    member: &'static str,
    leads: RangeInclusive<u8>,
    max_age_minutes: i64,
    now: DateTime<Utc>,
    overlap_cutoff: Option<DateTime<Utc>>,
) -> Result<Selection, ShortRangeError> {
    let (first_lead, last_lead) = (*leads.start(), *leads.end());
    let mut by_basetime: BTreeMap<DateTime<Utc>, BTreeSet<DateTime<Utc>>> = BTreeMap::new();

    for entry in entries {
        if entry.member.as_deref() != Some(member)
            || !entry.elements.iter().any(|element| element == ELEMENT)
        {
            continue;
        }

        let basetime = parse_jma_timestamp(&entry.basetime).map_err(|error| {
            ShortRangeError::UpstreamData(format!(
                "member {member}: invalid basetime {}: {error}",
                entry.basetime
            ))
        })?;
        let validtime = parse_jma_timestamp(&entry.validtime).map_err(|error| {
            ShortRangeError::UpstreamData(format!(
                "member {member}: invalid validtime {}: {error}",
                entry.validtime
            ))
        })?;

        by_basetime.entry(basetime).or_default().insert(validtime);
    }

    let mut future_complete = None;

    for (&basetime, validtimes) in by_basetime.iter().rev() {
        let mut frames = Vec::with_capacity(usize::from(last_lead - first_lead + 1));
        let mut complete = true;

        for lead in first_lead..=last_lead {
            let expected = basetime + Duration::hours(i64::from(lead));
            if validtimes.contains(&expected) {
                frames.push((lead, expected));
            } else if frames.is_empty() && overlap_cutoff.is_some_and(|cutoff| expected <= cutoff) {
                // JMA may omit a leading `none` frame whose end time is already
                // covered by the latest `immed` series.
            } else {
                complete = false;
                break;
            }
        }

        if !complete || frames.is_empty() {
            continue;
        }
        if basetime > now {
            future_complete = Some(basetime);
            continue;
        }
        if now.signed_duration_since(basetime) > Duration::minutes(max_age_minutes) {
            return Err(ShortRangeError::StaleData(format!(
                "member {member} basetime {basetime} is more than {max_age_minutes} minutes before {now}"
            )));
        }

        return Ok(Selection {
            member,
            basetime,
            validtimes: frames,
        });
    }

    if let Some(basetime) = future_complete {
        return Err(ShortRangeError::MissingTimes(format!(
            "member {member} has only a future complete basetime {basetime}"
        )));
    }

    Err(ShortRangeError::MissingTimes(format!(
        "no complete {first_lead}..{last_lead} hour series for member {member}"
    )))
}

async fn fetch_series(
    client: &reqwest::Client,
    base_url: &str,
    pixel: &Pixel,
    selection: &Selection,
) -> Result<ShortRangeSeries, ShortRangeError> {
    let basetime_jma = format_jma_timestamp(&selection.basetime);
    let mut samples = Vec::with_capacity(selection.validtimes.len());

    for &(lead_hours, validtime) in &selection.validtimes {
        let validtime_jma = format_jma_timestamp(&validtime);
        let url = format!(
            "{base_url}/{basetime_jma}/{member}/{validtime_jma}/surf/{ELEMENT}/{zoom}/{x}/{y}.png",
            member = selection.member,
            zoom = pixel.tile.z,
            x = pixel.tile.x,
            y = pixel.tile.y,
        );

        let body = fetch_bytes_with_retry(client, &url, MAX_TILE_BYTES).await?;
        let encoding = png_encoding(&body)?;
        let levels = decode_tile(&body)?;
        let index = usize::from(pixel.v) * TILE_SIZE as usize + usize::from(pixel.u);
        let level = *levels.get(index).ok_or_else(|| {
            ShortRangeError::UpstreamData(format!(
                "decoded tile has {} pixels, expected {}",
                levels.len(),
                TILE_SIZE * TILE_SIZE
            ))
        })?;
        let (status, min_mm, max_mm) = classify(level, encoding);

        let end = format_rfc3339(&validtime);
        let start = format_rfc3339(&(validtime - Duration::hours(1)));
        samples.push(ShortRangeSample {
            lead_hours,
            validtime: end.clone(),
            accumulation_start: start,
            accumulation_end: end,
            level,
            status,
            min_mm,
            max_mm,
        });
    }

    Ok(ShortRangeSeries {
        basetime: format_rfc3339(&selection.basetime),
        member: selection.member.to_string(),
        samples,
    })
}

/// Read the PNG IHDR colour type without decoding the image data.
fn png_encoding(data: &[u8]) -> Result<Encoding, ShortRangeError> {
    if !data.starts_with(b"\x89PNG\r\n\x1a\n") || data.get(12..16) != Some(&b"IHDR"[..]) {
        return Err(ShortRangeError::UpstreamData(
            "tile is not a PNG image".to_string(),
        ));
    }

    match data.get(25) {
        Some(2) => Ok(Encoding::Rgb),
        Some(3) => Ok(Encoding::Indexed),
        Some(6) => Ok(Encoding::Rgba),
        Some(other) => Err(ShortRangeError::UpstreamData(format!(
            "unsupported PNG colour type {other}"
        ))),
        None => Err(ShortRangeError::UpstreamData(
            "truncated PNG header".to_string(),
        )),
    }
}

fn classify(level: u8, encoding: Encoding) -> (ShortRangeStatus, Option<f64>, Option<f64>) {
    use ShortRangeStatus::*;

    match level {
        0 if encoding == Encoding::Indexed => (NoData, None, None),
        0 => (UnavailableOrBelowDisplayThreshold, None, None),
        1 => (BelowDisplayThreshold, None, None),
        // The lowest coloured class is [0, 1) mm for this one-hour product.
        2 => (Rain, Some(0.0), Some(1.0)),
        3..=9 => {
            let (min_mm, max_mm) = LEVEL_BOUNDS[usize::from(level)];
            (Rain, min_mm, max_mm)
        }
        _ => (NoData, None, None),
    }
}
