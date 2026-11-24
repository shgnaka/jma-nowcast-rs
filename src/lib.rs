//! Core library for the JMA high-resolution precipitation nowcast tiles.
//!
//! The crate exposes the low-level tile primitives together with the shared
//! query engine, HTTP client, cache, time handling and hourly short-range
//! forecast used by the CLI, REST API and MCP server.
//!
//! * [`tiles`] maps longitude/latitude coordinates to 256 px Web Mercator XYZ
//!   pixels and back to pixel centres.
//! * [`decode`] turns the JMA 4-bit indexed PNG tiles into a row-major array of
//!   precipitation levels.
//! * [`levels`] describes the precipitation level classes as ranges in mm/h.
//! * [`query`] resolves complete nowcast timelines and answers point, area,
//!   nearest-rain, route and rain-free-window queries.
//! * [`short_range`] implements the separate hourly `rasrf` forecast.
//! * [`client`] and [`cache`] provide the bounded HTTP/tile layer.
//! * [`times`] parses and selects JMA `targetTimes` entries.
//! * [`models`] holds shared attribution and request models.

pub mod cache;
pub mod client;
pub mod decode;
pub mod levels;
pub mod models;
pub mod query;
pub mod short_range;
pub mod tiles;
pub mod times;

pub use cache::{TileCache, TileCacheKey};
pub use client::{ClientError, JmaClient, TargetTimesKind, DEFAULT_JMA_BASE_URL};
pub use decode::{decode_tile, DecodeError};
pub use levels::{precipitation, LevelError, Precipitation, PrecipitationStatus, Status};
pub use models::{attach_attribution, Attribution, Waypoint};
pub use query::{Nowcast, NowcastError};
pub use short_range::{get_short_range, ShortRange, ShortRangeError, DEFAULT_SHORT_RANGE_BASE_URL};
pub use tiles::{locate, pixel_center, Pixel, TileCoord, TileError};
pub use times::{
    format_jma_timestamp, format_rfc3339, parse_jma_timestamp, TargetTimeEntry, TimeError,
};
