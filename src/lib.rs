//! Core primitives for the JMA high-resolution precipitation nowcast tiles.
//!
//! This crate currently implements requirements R1--R3:
//!
//! * [`tiles`] maps longitude/latitude coordinates to 256 px Web Mercator XYZ
//!   pixels and back to pixel centres.
//! * [`decode`] turns the JMA 4-bit indexed PNG tiles into a row-major array of
//!   precipitation levels.
//! * [`levels`] describes the precipitation level classes as ranges in mm/h.

pub mod decode;
pub mod levels;
pub mod tiles;

pub use decode::{decode_tile, DecodeError};
pub use levels::{precipitation, LevelError, Precipitation, PrecipitationStatus, Status};
pub use tiles::{locate, pixel_center, Pixel, TileCoord, TileError};
