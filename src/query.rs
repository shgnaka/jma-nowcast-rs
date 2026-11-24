//! Public query engine for JMA nowcast queries (R4, R7, R8, contract).

use crate::client::{ClientError, JmaClient, TargetTimesKind, DEFAULT_JMA_BASE_URL};
use crate::levels::precipitation;
use crate::models::attach_attribution;
pub use crate::models::Waypoint;
use crate::tiles::{
    check_zoom, locate, pixel_center, Pixel, TileCoord, TileError, MAX_MERCATOR_LAT, TILE_SIZE,
};
use crate::times::{
    format_rfc3339, select_latest_n1_analysis, select_nowcast_timeline_from_n1_n2, NowcastTimeline,
    TimeError,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Map, Value};
use std::fmt;

/// Max area bounds limits to prevent excessive queries (R7).
pub const MAX_AREA_WIDTH_DEG: f64 = 10.0;
pub const MAX_AREA_HEIGHT_DEG: f64 = 10.0;
pub const MAX_AREA_PIXELS: usize = 200_000;
pub const MAX_SEARCH_RADIUS_KM: f64 = 100.0;
pub const MAX_ROUTE_WAYPOINTS: usize = 1_000;

/// Standard unified error type per agy-contract.md.
#[derive(Debug)]
pub enum NowcastError {
    InvalidInput(String),
    StaleData(String),
    MissingTimes(String),
    UpstreamNotFound(String),
    UpstreamHttp(String),
    UpstreamData(String),
    Network(String),
}

impl NowcastError {
    pub fn code(&self) -> &'static str {
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

impl fmt::Display for NowcastError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(s) => write!(f, "[invalid_input] Invalid input: {s}"),
            Self::StaleData(s) => write!(f, "[stale_data] Stale data: {s}"),
            Self::MissingTimes(s) => write!(f, "[missing_times] Missing times: {s}"),
            Self::UpstreamNotFound(s) => write!(f, "[upstream_not_found] Upstream not found: {s}"),
            Self::UpstreamHttp(s) => write!(f, "[upstream_http] Upstream HTTP error: {s}"),
            Self::UpstreamData(s) => write!(f, "[upstream_data] Upstream data error: {s}"),
            Self::Network(s) => write!(f, "[network] Network error: {s}"),
        }
    }
}

impl std::error::Error for NowcastError {}

impl From<TileError> for NowcastError {
    fn from(err: TileError) -> Self {
        Self::InvalidInput(err.to_string())
    }
}

impl From<TimeError> for NowcastError {
    fn from(err: TimeError) -> Self {
        match err {
            TimeError::InvalidFormat(s) => Self::UpstreamData(format!("invalid time format: {s}")),
            TimeError::StaleData { basetime, now } => Self::StaleData(format!(
                "basetime {basetime} is older than 20 minutes from now {now}"
            )),
            TimeError::FutureBasetime { basetime, now } => Self::StaleData(format!(
                "basetime {basetime} is in the future relative to now {now}"
            )),
            TimeError::MissingTimes(s) => Self::MissingTimes(s),
            TimeError::NoCompleteSeries => {
                Self::MissingTimes("no complete 0..60 minute nowcast series found".to_string())
            }
        }
    }
}

impl From<ClientError> for NowcastError {
    fn from(err: ClientError) -> Self {
        match err {
            ClientError::InvalidInput(s) => Self::InvalidInput(s),
            ClientError::UpstreamNotFound(s) => Self::UpstreamNotFound(s),
            ClientError::UpstreamHttp(s) => Self::UpstreamHttp(s),
            ClientError::UpstreamData(s) => Self::UpstreamData(s),
            ClientError::Network(s) => Self::Network(s),
        }
    }
}

/// Nowcast query engine.
#[derive(Clone)]
pub struct Nowcast {
    client: JmaClient,
    fixed_now: Option<DateTime<Utc>>,
}

impl Nowcast {
    /// Create a new Nowcast engine.
    /// If base_url is empty, uses DEFAULT_JMA_BASE_URL.
    pub fn new(base_url: &str, fixed_now: Option<DateTime<Utc>>) -> Result<Self, NowcastError> {
        let url = if base_url.is_empty() {
            DEFAULT_JMA_BASE_URL
        } else {
            base_url
        };
        // 512 tile capacity cache
        let client = JmaClient::new(url, 512)?;
        Ok(Self { client, fixed_now })
    }

    pub fn now(&self) -> DateTime<Utc> {
        self.fixed_now.unwrap_or_else(Utc::now)
    }

    async fn get_n1_analysis_basetime(&self) -> Result<DateTime<Utc>, NowcastError> {
        let n1_entries = self.client.fetch_target_times(TargetTimesKind::N1).await?;
        let bt = select_latest_n1_analysis(&n1_entries, self.now())?;
        Ok(bt)
    }

    async fn get_full_timeline(&self) -> Result<NowcastTimeline, NowcastError> {
        let n1_entries = self.client.fetch_target_times(TargetTimesKind::N1).await?;
        let n2_entries = self.client.fetch_target_times(TargetTimesKind::N2).await?;
        let timeline = select_nowcast_timeline_from_n1_n2(&n1_entries, &n2_entries, self.now())?;
        Ok(timeline)
    }

    async fn sample_pixel_raw(
        &self,
        basetime: DateTime<Utc>,
        validtime: DateTime<Utc>,
        pixel: Pixel,
    ) -> Result<u8, NowcastError> {
        let tile_data = self
            .client
            .get_hrpns_tile(basetime, validtime, pixel.tile)
            .await?;
        let expected_size = (TILE_SIZE as usize) * (TILE_SIZE as usize);
        if tile_data.len() != expected_size {
            return Err(NowcastError::UpstreamData(format!(
                "decoded tile has invalid size: {} != {}",
                tile_data.len(),
                expected_size
            )));
        }
        let idx = pixel.v as usize * (TILE_SIZE as usize) + pixel.u as usize;
        let lvl = tile_data
            .get(idx)
            .copied()
            .ok_or_else(|| NowcastError::UpstreamData(format!("pixel {pixel:?} out of bounds")))?;
        Ok(lvl)
    }

    /// Point precipitation observation (validtime == basetime)
    pub async fn get_precipitation(
        &self,
        lat: f64,
        lon: f64,
        zoom: u8,
    ) -> Result<Value, NowcastError> {
        check_zoom(zoom)?;
        let pixel = locate(lat, lon, zoom)?;
        let basetime = self.get_n1_analysis_basetime().await?;

        let level = self.sample_pixel_raw(basetime, basetime, pixel).await?;
        let precip = precipitation(level).map_err(|e| NowcastError::UpstreamData(e.to_string()))?;

        let mut res = Map::new();
        attach_attribution(&mut res);
        res.insert("basetime".to_string(), json!(format_rfc3339(&basetime)));
        res.insert("validtime".to_string(), json!(format_rfc3339(&basetime)));
        res.insert("precipitation".to_string(), json!(precip));

        Ok(Value::Object(res))
    }

    /// Point nowcast forecast (0..60m in 5-minute steps)
    pub async fn get_nowcast(&self, lat: f64, lon: f64, zoom: u8) -> Result<Value, NowcastError> {
        check_zoom(zoom)?;
        let pixel = locate(lat, lon, zoom)?;
        let timeline = self.get_full_timeline().await?;

        let mut samples = Vec::new();
        for &vt in &timeline.forecast_validtimes {
            let level = self.sample_pixel_raw(timeline.basetime, vt, pixel).await?;
            let precip =
                precipitation(level).map_err(|e| NowcastError::UpstreamData(e.to_string()))?;
            samples.push(json!({
                "validtime": format_rfc3339(&vt),
                "precipitation": precip,
            }));
        }

        let mut res = Map::new();
        attach_attribution(&mut res);
        res.insert(
            "basetime".to_string(),
            json!(format_rfc3339(&timeline.basetime)),
        );
        res.insert("samples".to_string(), Value::Array(samples));

        Ok(Value::Object(res))
    }

    /// Area nowcast: pixel counts across all 10 precipitation levels.
    pub async fn get_area_nowcast(
        &self,
        south: f64,
        west: f64,
        north: f64,
        east: f64,
        zoom: u8,
        lead_minutes: u8,
    ) -> Result<Value, NowcastError> {
        check_zoom(zoom)?;

        if !south.is_finite() || !north.is_finite() || !west.is_finite() || !east.is_finite() {
            return Err(NowcastError::InvalidInput(
                "coordinates must be finite".to_string(),
            ));
        }
        if south > north {
            return Err(NowcastError::InvalidInput(
                "south latitude cannot be greater than north latitude".to_string(),
            ));
        }
        if west > east {
            return Err(NowcastError::InvalidInput(
                "bounding box crossing antimeridian is not supported".to_string(),
            ));
        }
        if (north - south) > MAX_AREA_HEIGHT_DEG || (east - west) > MAX_AREA_WIDTH_DEG {
            return Err(NowcastError::InvalidInput(
                "requested area bounds exceed maximum allowable dimensions".to_string(),
            ));
        }
        if lead_minutes > 60 || !lead_minutes.is_multiple_of(5) {
            return Err(NowcastError::InvalidInput(
                "lead_minutes must be between 0 and 60 and divisible by 5".to_string(),
            ));
        }

        let tl = locate(north, west, zoom)?;
        let br = locate(south, east, zoom)?;

        let min_gx = tl.tile.x as u64 * TILE_SIZE as u64 + tl.u as u64;
        let max_gx = br.tile.x as u64 * TILE_SIZE as u64 + br.u as u64;
        let min_gy = tl.tile.y as u64 * TILE_SIZE as u64 + tl.v as u64;
        let max_gy = br.tile.y as u64 * TILE_SIZE as u64 + br.v as u64;

        let width = max_gx.saturating_sub(min_gx) + 1;
        let height = max_gy.saturating_sub(min_gy) + 1;
        let total_pixels = width * height;

        if total_pixels > MAX_AREA_PIXELS as u64 {
            return Err(NowcastError::InvalidInput(format!(
                "requested area covers {total_pixels} pixels, exceeding limit of {MAX_AREA_PIXELS}"
            )));
        }

        let (basetime, target_vt) = if lead_minutes == 0 {
            let bt = self.get_n1_analysis_basetime().await?;
            (bt, bt)
        } else {
            let tl = self.get_full_timeline().await?;
            let vt = tl.basetime + Duration::minutes(lead_minutes as i64);
            (tl.basetime, vt)
        };

        // Required tiles
        let min_tx = (min_gx / TILE_SIZE as u64) as u32;
        let max_tx = (max_gx / TILE_SIZE as u64) as u32;
        let min_ty = (min_gy / TILE_SIZE as u64) as u32;
        let max_ty = (max_gy / TILE_SIZE as u64) as u32;

        let mut counts = [0_u64; 10];

        for ty in min_ty..=max_ty {
            for tx in min_tx..=max_tx {
                let tile = TileCoord::new(zoom, tx, ty);
                let tile_data = self
                    .client
                    .get_hrpns_tile(basetime, target_vt, tile)
                    .await?;

                let expected_size = (TILE_SIZE as usize) * (TILE_SIZE as usize);
                if tile_data.len() != expected_size {
                    return Err(NowcastError::UpstreamData(format!(
                        "decoded tile has invalid size: {} != {}",
                        tile_data.len(),
                        expected_size
                    )));
                }

                let tile_min_gx = tx as u64 * TILE_SIZE as u64;
                let tile_max_gx = tile_min_gx + (TILE_SIZE as u64) - 1;
                let tile_min_gy = ty as u64 * TILE_SIZE as u64;
                let tile_max_gy = tile_min_gy + (TILE_SIZE as u64) - 1;

                let overlap_min_x = min_gx.max(tile_min_gx);
                let overlap_max_x = max_gx.min(tile_max_gx);
                let overlap_min_y = min_gy.max(tile_min_gy);
                let overlap_max_y = max_gy.min(tile_max_gy);

                for gy in overlap_min_y..=overlap_max_y {
                    let v = (gy - tile_min_gy) as usize;
                    for gx in overlap_min_x..=overlap_max_x {
                        let u = (gx - tile_min_gx) as usize;
                        let lvl = tile_data[v * TILE_SIZE as usize + u];
                        if (lvl as usize) < counts.len() {
                            counts[lvl as usize] += 1;
                        }
                    }
                }
            }
        }

        let mut res = Map::new();
        attach_attribution(&mut res);
        res.insert("basetime".to_string(), json!(format_rfc3339(&basetime)));
        res.insert("validtime".to_string(), json!(format_rfc3339(&target_vt)));
        res.insert("counts".to_string(), json!(counts));

        Ok(Value::Object(res))
    }

    /// Nearest rain search within radius_km (levels 2..=9).
    pub async fn get_nearest_rain(
        &self,
        lat: f64,
        lon: f64,
        radius_km: f64,
        zoom: u8,
        lead_minutes: u8,
    ) -> Result<Value, NowcastError> {
        check_zoom(zoom)?;

        if !lat.is_finite() || !lon.is_finite() || !radius_km.is_finite() {
            return Err(NowcastError::InvalidInput(
                "coordinates and radius must be finite".to_string(),
            ));
        }
        if radius_km <= 0.0 || radius_km > MAX_SEARCH_RADIUS_KM {
            return Err(NowcastError::InvalidInput(format!(
                "radius_km must be > 0 and <= {MAX_SEARCH_RADIUS_KM}"
            )));
        }
        if lead_minutes > 60 || !lead_minutes.is_multiple_of(5) {
            return Err(NowcastError::InvalidInput(
                "lead_minutes must be between 0 and 60 and divisible by 5".to_string(),
            ));
        }

        locate(lat, lon, zoom)?; // validate location

        // Exact spherical bounds for search radius
        const EARTH_RADIUS_KM: f64 = 6371.0;
        let angular_dist = radius_km / EARTH_RADIUS_KM;
        let phi = lat.to_radians();

        let min_lat_rad = phi - angular_dist;
        let max_lat_rad = phi + angular_dist;

        let min_lat = (min_lat_rad.to_degrees()).max(-MAX_MERCATOR_LAT);
        let max_lat = (max_lat_rad.to_degrees()).min(MAX_MERCATOR_LAT);

        let delta_lon_rad = if phi.abs() + angular_dist >= std::f64::consts::FRAC_PI_2 {
            std::f64::consts::PI
        } else {
            (angular_dist.sin() / phi.cos()).asin()
        };
        let delta_lon = delta_lon_rad.to_degrees();

        let min_lon_raw = lon - delta_lon;
        let max_lon_raw = lon + delta_lon;

        // Longitude segments supporting date line crossing
        let lon_segments: Vec<(f64, f64)> = if min_lon_raw < -180.0 {
            vec![
                (-180.0, max_lon_raw.min(180.0)),
                (min_lon_raw + 360.0, 180.0),
            ]
        } else if max_lon_raw > 180.0 {
            vec![
                (min_lon_raw.max(-180.0), 180.0),
                (-180.0, max_lon_raw - 360.0),
            ]
        } else {
            vec![(min_lon_raw, max_lon_raw)]
        };

        // Pre-validate pixel count across all segments before fetching
        let mut total_pixels: u64 = 0;
        let mut segment_ranges = Vec::new();

        for &(seg_w, seg_e) in &lon_segments {
            let tl = locate(max_lat, seg_w, zoom)?;
            let br = locate(min_lat, seg_e, zoom)?;

            let min_gx = tl.tile.x as u64 * TILE_SIZE as u64 + tl.u as u64;
            let max_gx = br.tile.x as u64 * TILE_SIZE as u64 + br.u as u64;
            let min_gy = tl.tile.y as u64 * TILE_SIZE as u64 + tl.v as u64;
            let max_gy = br.tile.y as u64 * TILE_SIZE as u64 + br.v as u64;

            let w = max_gx.saturating_sub(min_gx) + 1;
            let h = max_gy.saturating_sub(min_gy) + 1;
            total_pixels += w * h;
            segment_ranges.push((min_gx, max_gx, min_gy, max_gy));
        }

        if total_pixels > MAX_AREA_PIXELS as u64 {
            return Err(NowcastError::InvalidInput(format!(
                "nearest search bounding box covers {total_pixels} pixels, exceeding limit of {MAX_AREA_PIXELS}"
            )));
        }

        let (basetime, target_vt) = if lead_minutes == 0 {
            let bt = self.get_n1_analysis_basetime().await?;
            (bt, bt)
        } else {
            let tl = self.get_full_timeline().await?;
            let vt = tl.basetime + Duration::minutes(lead_minutes as i64);
            (tl.basetime, vt)
        };

        let mut nearest: Option<(f64, f64, f64, f64)> = None; // (dist, bearing, clat, clon)
        let mut missing_pixels = 0_u64;

        for (min_gx, max_gx, min_gy, max_gy) in segment_ranges {
            let min_tx = (min_gx / TILE_SIZE as u64) as u32;
            let max_tx = (max_gx / TILE_SIZE as u64) as u32;
            let min_ty = (min_gy / TILE_SIZE as u64) as u32;
            let max_ty = (max_gy / TILE_SIZE as u64) as u32;

            for ty in min_ty..=max_ty {
                for tx in min_tx..=max_tx {
                    let tile = TileCoord::new(zoom, tx, ty);
                    let tile_data = self
                        .client
                        .get_hrpns_tile(basetime, target_vt, tile)
                        .await?;

                    let expected_size = (TILE_SIZE as usize) * (TILE_SIZE as usize);
                    if tile_data.len() != expected_size {
                        return Err(NowcastError::UpstreamData(format!(
                            "decoded tile has invalid size: {} != {}",
                            tile_data.len(),
                            expected_size
                        )));
                    }

                    let tile_min_gx = tx as u64 * TILE_SIZE as u64;
                    let tile_max_gx = tile_min_gx + (TILE_SIZE as u64) - 1;
                    let tile_min_gy = ty as u64 * TILE_SIZE as u64;
                    let tile_max_gy = tile_min_gy + (TILE_SIZE as u64) - 1;

                    let overlap_min_x = min_gx.max(tile_min_gx);
                    let overlap_max_x = max_gx.min(tile_max_gx);
                    let overlap_min_y = min_gy.max(tile_min_gy);
                    let overlap_max_y = max_gy.min(tile_max_gy);

                    for gy in overlap_min_y..=overlap_max_y {
                        let v = (gy - tile_min_gy) as u8;
                        for gx in overlap_min_x..=overlap_max_x {
                            let u = (gx - tile_min_gx) as u8;
                            let pixel = Pixel::new(tile, u, v);
                            let lvl = tile_data[v as usize * TILE_SIZE as usize + u as usize];

                            let (clat, clon) = pixel_center(pixel)?;
                            let (dist, bearing) = haversine_and_bearing(lat, lon, clat, clon);

                            if dist <= radius_km {
                                if lvl == 0 {
                                    missing_pixels += 1;
                                } else if lvl >= 2 {
                                    match &nearest {
                                        None => nearest = Some((dist, bearing, clat, clon)),
                                        Some((best_dist, _, _, _)) if dist < *best_dist => {
                                            nearest = Some((dist, bearing, clat, clon));
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let mut res = Map::new();
        attach_attribution(&mut res);
        res.insert("basetime".to_string(), json!(format_rfc3339(&basetime)));
        res.insert("validtime".to_string(), json!(format_rfc3339(&target_vt)));
        res.insert("missing_pixels".to_string(), json!(missing_pixels));

        if let Some((dist, bearing, clat, clon)) = nearest {
            let rounded_dist = (dist * 1000.0).round() / 1000.0;
            let mut rounded_bearing = (bearing * 10.0).round() / 10.0;
            if rounded_bearing >= 360.0 {
                rounded_bearing -= 360.0;
            }
            res.insert(
                "nearest".to_string(),
                json!({
                    "distance_km": rounded_dist,
                    "bearing_degrees": rounded_bearing,
                    "lat": clat,
                    "lon": clon,
                }),
            );
        } else {
            res.insert("nearest".to_string(), Value::Null);
        }

        Ok(Value::Object(res))
    }

    /// Route nowcast: evaluates precipitation along a route of timed waypoints (R8).
    pub async fn get_route_nowcast(
        &self,
        points: Vec<Waypoint>,
        zoom: u8,
    ) -> Result<Value, NowcastError> {
        check_zoom(zoom)?;

        if points.is_empty() {
            return Err(NowcastError::InvalidInput(
                "points cannot be empty".to_string(),
            ));
        }
        if points.len() > MAX_ROUTE_WAYPOINTS {
            return Err(NowcastError::InvalidInput(format!(
                "points count {} exceeds maximum allowed ({MAX_ROUTE_WAYPOINTS})",
                points.len()
            )));
        }

        let timeline = self.get_full_timeline().await?;
        let basetime = timeline.basetime;
        let max_time = basetime + Duration::minutes(60);

        // Pre-validate all waypoints and range before fetching any tiles
        let mut pre_validated = Vec::with_capacity(points.len());
        for pt in &points {
            if pt.time < basetime || pt.time > max_time {
                return Err(NowcastError::InvalidInput(format!(
                    "waypoint time {} is outside the 0..60m forecast window ({basetime} .. {max_time})",
                    pt.time
                )));
            }

            let pixel = locate(pt.lat, pt.lon, zoom)?;

            let diff_mins = (pt.time.signed_duration_since(basetime)).num_minutes();
            let slot_step = (diff_mins / 5) * 5;
            let validtime = basetime + Duration::minutes(slot_step);
            pre_validated.push((pt, pixel, validtime));
        }

        let mut samples = Vec::with_capacity(points.len());

        for (pt, pixel, validtime) in pre_validated {
            let level = self.sample_pixel_raw(basetime, validtime, pixel).await?;
            let precip =
                precipitation(level).map_err(|e| NowcastError::UpstreamData(e.to_string()))?;

            samples.push(json!({
                "requested_time": format_rfc3339(&pt.time),
                "validtime": format_rfc3339(&validtime),
                "lat": pt.lat,
                "lon": pt.lon,
                "precipitation": precip,
            }));
        }

        let mut res = Map::new();
        attach_attribution(&mut res);
        res.insert(
            "basetime".to_string(),
            json!(format_rfc3339(&timeline.basetime)),
        );
        res.insert("samples".to_string(), Value::Array(samples));

        Ok(Value::Object(res))
    }

    /// Dry windows: consecutive level 1 (no rain) 5-minute slots (R8).
    pub async fn get_rain_free_windows(
        &self,
        lat: f64,
        lon: f64,
        zoom: u8,
    ) -> Result<Value, NowcastError> {
        check_zoom(zoom)?;
        let pixel = locate(lat, lon, zoom)?;
        let timeline = self.get_full_timeline().await?;
        let max_end = timeline.basetime + Duration::minutes(60);

        // Positive-duration forecast intervals only: validtime < max_end (exclude +60m)
        let mut slots = Vec::new();
        for &vt in &timeline.forecast_validtimes {
            if vt < max_end {
                let level = self.sample_pixel_raw(timeline.basetime, vt, pixel).await?;
                slots.push((vt, level));
            }
        }

        let mut windows = Vec::new();

        let mut current_start: Option<DateTime<Utc>> = None;
        let mut current_end: Option<DateTime<Utc>> = None;

        for (vt, lvl) in slots {
            if lvl == 1 {
                // Dry slot: represents [vt, min(vt + 5m, max_end)]
                let slot_start = vt;
                let slot_end = (vt + Duration::minutes(5)).min(max_end);

                if slot_end > slot_start {
                    match current_start {
                        None => {
                            current_start = Some(slot_start);
                            current_end = Some(slot_end);
                        }
                        Some(_) => {
                            // Consecutive slot
                            current_end = Some(slot_end);
                        }
                    }
                }
                if slot_end >= max_end {
                    break;
                }
            } else {
                // Rain (>= 2) or NoData (0) terminates current dry window
                if let (Some(s), Some(e)) = (current_start, current_end) {
                    if e > s {
                        windows.push(json!({
                            "start": format_rfc3339(&s),
                            "end": format_rfc3339(&e),
                        }));
                    }
                    current_start = None;
                    current_end = None;
                }
            }
        }

        if let (Some(s), Some(e)) = (current_start, current_end) {
            if e > s {
                windows.push(json!({
                    "start": format_rfc3339(&s),
                    "end": format_rfc3339(&e),
                }));
            }
        }

        let mut res = Map::new();
        attach_attribution(&mut res);
        res.insert(
            "basetime".to_string(),
            json!(format_rfc3339(&timeline.basetime)),
        );
        res.insert("windows".to_string(), Value::Array(windows));

        Ok(Value::Object(res))
    }
}

/// Great circle distance in km and initial bearing in degrees (clockwise from true north 0..360)
fn haversine_and_bearing(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> (f64, f64) {
    const EARTH_RADIUS_KM: f64 = 6371.0;

    let phi1 = lat1.to_radians();
    let phi2 = lat2.to_radians();
    let delta_phi = (lat2 - lat1).to_radians();
    let delta_lambda = (lon2 - lon1).to_radians();

    let a = (delta_phi / 2.0).sin().powi(2)
        + phi1.cos() * phi2.cos() * (delta_lambda / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().asin();
    let distance = EARTH_RADIUS_KM * c;

    let y = delta_lambda.sin() * phi2.cos();
    let x = phi1.cos() * phi2.sin() - phi1.sin() * phi2.cos() * delta_lambda.cos();
    let bearing_rad = y.atan2(x);
    let mut bearing_deg = bearing_rad.to_degrees();
    if bearing_deg < 0.0 {
        bearing_deg += 360.0;
    }
    if bearing_deg >= 360.0 {
        bearing_deg -= 360.0;
    }

    (distance, bearing_deg)
}
