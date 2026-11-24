//! Attributions, common models, and shared structs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const JMA_SOURCE: &str = "気象庁";
pub const JMA_SOURCE_URL: &str = "https://www.jma.go.jp/bosai/nowc/";
pub const PROCESSED_BY: &str = "jma-nowcast-rs";
pub const PROCESSING_DESC: &str = "気象庁の高解像度降水ナウキャストタイルデータを取得・階級判定・地理空間サンプリング加工したものです。";
pub const LICENSE_NAME: &str = "PDL-1.0";

/// Standard attribution metadata included in all responses (R6).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attribution {
    pub source: String,
    pub source_url: String,
    pub processed: bool,
    pub processing: String,
    pub license: String,
}

impl Default for Attribution {
    fn default() -> Self {
        Self {
            source: JMA_SOURCE.to_string(),
            source_url: JMA_SOURCE_URL.to_string(),
            processed: true,
            processing: PROCESSING_DESC.to_string(),
            license: LICENSE_NAME.to_string(),
        }
    }
}

/// Attach standard attribution fields to a serde_json::Map or Value.
pub fn attach_attribution(map: &mut serde_json::Map<String, serde_json::Value>) {
    let attr = Attribution::default();
    if let serde_json::Value::Object(m) = serde_json::to_value(attr).unwrap_or_default() {
        for (k, v) in m {
            map.insert(k, v);
        }
    }
}

/// Waypoint used in route nowcast queries.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Waypoint {
    pub lat: f64,
    pub lon: f64,
    pub time: DateTime<Utc>,
}
