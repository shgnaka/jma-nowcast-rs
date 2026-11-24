//! Web Mercator XYZ tile and pixel mathematics for the JMA nowcast tiles.
//!
//! Tiles are standard 256 x 256 Web Mercator (EPSG:3857) XYZ tiles. Only the
//! zooms that actually carry JMA ``hrpns`` data are accepted: 4, 6, 8 and 10.

use std::f64::consts::PI;
use std::fmt;

/// Width and height of a tile in pixels.
pub const TILE_SIZE: u32 = 256;

/// Zoom levels that carry JMA high-resolution precipitation nowcast data.
pub const VALID_ZOOMS: [u8; 4] = [4, 6, 8, 10];

/// Default zoom used by higher-level APIs.
pub const DEFAULT_ZOOM: u8 = 10;

/// Alias for [`DEFAULT_ZOOM`].
pub const DEFAULT_Z: u8 = DEFAULT_ZOOM;

/// Maximum absolute latitude representable in Web Mercator.
pub const MAX_MERCATOR_LAT: f64 = 85.051_128_779_806_6;

/// A tile coordinate (XYZ scheme).
///
/// `x` increases eastwards and `y` increases southwards. All fields are public
/// so callers can construct coordinates directly.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct TileCoord {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileCoord {
    /// Construct a tile coordinate without validating it.
    pub const fn new(z: u8, x: u32, y: u32) -> Self {
        Self { z, x, y }
    }
}

/// A single pixel inside a tile.
///
/// `u` is the column (0..=255, west to east) and `v` is the row (0..=255,
/// north to south). All fields are public so callers can construct pixels
/// directly.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Pixel {
    pub tile: TileCoord,
    pub u: u8,
    pub v: u8,
}

impl Pixel {
    /// Construct a pixel without validating it.
    pub const fn new(tile: TileCoord, u: u8, v: u8) -> Self {
        Self { tile, u, v }
    }
}

/// Errors returned by the tile maths.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum TileError {
    /// The zoom is not one of [`VALID_ZOOMS`].
    InvalidZoom(u8),
    /// The latitude is not finite or lies outside the Web Mercator domain.
    InvalidLatitude(f64),
    /// The longitude is not finite or lies outside `-180..=180`.
    InvalidLongitude(f64),
    /// The tile index is outside the valid range for its zoom.
    InvalidTile(TileCoord),
    /// The pixel is outside the valid range for its tile.
    InvalidPixel(Pixel),
}

impl fmt::Display for TileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidZoom(z) => write!(
                f,
                "zoom {z} has no hrpns data; valid zooms are {VALID_ZOOMS:?}"
            ),
            Self::InvalidLatitude(lat) => {
                write!(f, "latitude {lat} is not projectable to Web Mercator")
            }
            Self::InvalidLongitude(lon) => write!(f, "longitude {lon} is outside -180..=180"),
            Self::InvalidTile(tile) => write!(
                f,
                "tile ({}, {}, {}) is outside the XYZ range for its zoom",
                tile.z, tile.x, tile.y
            ),
            Self::InvalidPixel(pixel) => write!(
                f,
                "pixel ({}, {}) is outside tile ({}, {}, {})",
                pixel.u, pixel.v, pixel.tile.z, pixel.tile.x, pixel.tile.y
            ),
        }
    }
}

impl std::error::Error for TileError {}

/// Alias for [`TileError`].
pub type Error = TileError;

/// Validate a zoom level.
pub fn check_zoom(z: u8) -> Result<u8, TileError> {
    if VALID_ZOOMS.contains(&z) {
        Ok(z)
    } else {
        Err(TileError::InvalidZoom(z))
    }
}

/// Convert a `(latitude, longitude)` pair in degrees to the pixel containing
/// it at zoom `z`.
///
/// Latitude must be finite and within the Web Mercator range
/// (`-85.0511287798066..=85.0511287798066`), and longitude must be finite and
/// within `-180..=180`. Coordinates exactly on the world edge are assigned to
/// the final valid pixel rather than being rejected or wrapping around.
pub fn locate(lat: f64, lon: f64, z: u8) -> Result<Pixel, TileError> {
    check_zoom(z)?;

    if !lat.is_finite() || !(-MAX_MERCATOR_LAT..=MAX_MERCATOR_LAT).contains(&lat) {
        return Err(TileError::InvalidLatitude(lat));
    }
    if !lon.is_finite() || !(-180.0..=180.0).contains(&lon) {
        return Err(TileError::InvalidLongitude(lon));
    }

    let side = 1_u64 << z;
    let global_size = (TILE_SIZE as u64) * side;
    let n = global_size as f64;

    let px = (lon + 180.0) / 360.0 * n;
    let sin_lat = lat.to_radians().sin();
    let py = (0.5 - ((1.0 + sin_lat) / (1.0 - sin_lat)).ln() / (4.0 * PI)) * n;

    // `clamp` keeps longitude 180 and latitude -MAX_MERCATOR_LAT on the last
    // valid pixel of the world. The value is exact for the supported zooms.
    let ix = px.floor().clamp(0.0, (global_size - 1) as f64) as u64;
    let iy = py.floor().clamp(0.0, (global_size - 1) as f64) as u64;

    let tile_size = TILE_SIZE as u64;
    Ok(Pixel {
        tile: TileCoord {
            z,
            x: (ix / tile_size) as u32,
            y: (iy / tile_size) as u32,
        },
        u: (ix % tile_size) as u8,
        v: (iy % tile_size) as u8,
    })
}

/// Return the `(latitude, longitude)` of the centre of `pixel` in degrees.
///
/// This is the inverse of [`locate`] for valid pixels: applying [`locate`] to
/// the returned centre gives back the same pixel.
pub fn pixel_center(pixel: Pixel) -> Result<(f64, f64), TileError> {
    check_zoom(pixel.tile.z)?;

    let max_tile = (1_u32 << pixel.tile.z) - 1;
    if pixel.tile.x > max_tile || pixel.tile.y > max_tile {
        return Err(TileError::InvalidTile(pixel.tile));
    }

    let n = ((TILE_SIZE as u64) << pixel.tile.z) as f64;
    let gx = pixel.tile.x as f64 * TILE_SIZE as f64 + pixel.u as f64 + 0.5;
    let gy = pixel.tile.y as f64 * TILE_SIZE as f64 + pixel.v as f64 + 0.5;

    let lon = gx / n * 360.0 - 180.0;
    let lat = (PI * (1.0 - 2.0 * gy / n)).sinh().atan().to_degrees();
    Ok((lat, lon))
}
