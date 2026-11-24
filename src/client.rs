//! HTTP client for fetching JMA target times and tiles (R5).

use crate::cache::{TileCache, TileCacheKey};
use crate::decode::decode_tile;
use crate::tiles::TileCoord;
use crate::times::{format_jma_timestamp, TargetTimeEntry};
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

pub const DEFAULT_JMA_BASE_URL: &str = "https://www.jma.go.jp/bosai/jmatile/data/nowc";
pub const MAX_JSON_BODY_SIZE: usize = 2 * 1024 * 1024; // 2 MB cap for targetTimes
pub const MAX_TILE_BODY_SIZE: usize = 1024 * 1024; // 1 MB cap for PNG tiles

const ATTEMPTS: usize = 3;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetTimesKind {
    N1,
    N2,
}

#[derive(Debug)]
pub enum ClientError {
    InvalidInput(String),
    UpstreamNotFound(String),
    UpstreamHttp(String),
    UpstreamData(String),
    Network(String),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(s) => write!(f, "invalid input: {s}"),
            Self::UpstreamNotFound(s) => write!(f, "upstream not found: {s}"),
            Self::UpstreamHttp(s) => write!(f, "upstream http error: {s}"),
            Self::UpstreamData(s) => write!(f, "upstream data decode error: {s}"),
            Self::Network(s) => write!(f, "network error: {s}"),
        }
    }
}

impl std::error::Error for ClientError {}

#[derive(Clone)]
pub struct JmaClient {
    base_url: String,
    http_client: reqwest::Client,
    cache: TileCache,
}

impl JmaClient {
    pub fn new(base_url: &str, cache_capacity: usize) -> Result<Self, ClientError> {
        let http_client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| ClientError::Network(e.to_string()))?;

        let url = base_url.trim_end_matches('/').to_string();

        Ok(Self {
            base_url: url,
            http_client,
            cache: TileCache::new(cache_capacity),
        })
    }

    /// Fetch targetTimes_N1.json or targetTimes_N2.json.
    pub async fn fetch_target_times(
        &self,
        kind: TargetTimesKind,
    ) -> Result<Vec<TargetTimeEntry>, ClientError> {
        let filename = match kind {
            TargetTimesKind::N1 => "targetTimes_N1.json",
            TargetTimesKind::N2 => "targetTimes_N2.json",
        };
        let url = format!("{}/{}", self.base_url, filename);
        let bytes = fetch_bytes_with_retry(&self.http_client, &url, MAX_JSON_BODY_SIZE).await?;
        serde_json::from_slice::<Vec<TargetTimeEntry>>(&bytes)
            .map_err(|e| ClientError::UpstreamData(format!("failed to parse {filename}: {e}")))
    }

    /// Fetch and decode a tile, utilizing the bounded memory cache.
    /// URL format: `{base_url}/{basetime}/none/{validtime}/surf/hrpns/{z}/{x}/{y}.png`
    pub async fn get_hrpns_tile(
        &self,
        basetime: DateTime<Utc>,
        validtime: DateTime<Utc>,
        tile: TileCoord,
    ) -> Result<Arc<Vec<u8>>, ClientError> {
        let key = TileCacheKey {
            basetime,
            validtime,
            tile,
        };

        if let Some(cached) = self.cache.get(&key) {
            return Ok(cached);
        }

        let bt_str = format_jma_timestamp(&basetime);
        let vt_str = format_jma_timestamp(&validtime);
        let url = format!(
            "{}/{}/none/{}/surf/hrpns/{}/{}/{}.png",
            self.base_url, bt_str, vt_str, tile.z, tile.x, tile.y
        );

        let bytes = fetch_bytes_with_retry(&self.http_client, &url, MAX_TILE_BODY_SIZE).await?;

        let decoded = decode_tile(&bytes).map_err(|e| {
            ClientError::UpstreamData(format!("failed to decode tile {tile:?}: {e}"))
        })?;

        let arc_data = Arc::new(decoded);
        self.cache.insert(key, arc_data.clone());
        Ok(arc_data)
    }
}

/// Fetch one HTTP resource with a streaming size cap and bounded retries.
///
/// Missing resources (404) and non-server client errors are final. Server
/// errors, connection failures and truncated bodies are retried up to three
/// times with a ten-second timeout per attempt.
pub(crate) async fn fetch_bytes_with_retry(
    client: &reqwest::Client,
    url: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, ClientError> {
    let mut last_error = None;

    for attempt in 0..ATTEMPTS {
        let mut response = match client.get(url).timeout(REQUEST_TIMEOUT).send().await {
            Ok(response) => response,
            Err(error) => {
                last_error = Some(ClientError::Network(format!("{url}: {error}")));
                continue;
            }
        };

        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Err(ClientError::UpstreamNotFound(format!(
                "resource not found (404) at {url}"
            )));
        }
        if !status.is_success() {
            let error = ClientError::UpstreamHttp(format!("upstream returned {status} for {url}"));
            if status.is_server_error() && attempt + 1 < ATTEMPTS {
                last_error = Some(error);
                continue;
            }
            return Err(error);
        }

        if let Some(length) = response.content_length() {
            if length > max_bytes as u64 {
                return Err(ClientError::UpstreamData(format!(
                    "response body exceeds cap of {max_bytes} bytes at {url}"
                )));
            }
        }

        let mut body = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    if body.len().saturating_add(chunk.len()) > max_bytes {
                        return Err(ClientError::UpstreamData(format!(
                            "response body exceeds cap of {max_bytes} bytes at {url}"
                        )));
                    }
                    body.extend_from_slice(&chunk);
                }
                Ok(None) => return Ok(body),
                Err(error) => {
                    last_error = Some(ClientError::Network(format!("{url}: {error}")));
                    break;
                }
            }
        }
    }

    Err(last_error.unwrap_or_else(|| ClientError::Network(format!("request failed: {url}"))))
}
