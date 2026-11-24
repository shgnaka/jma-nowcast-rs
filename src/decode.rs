//! Decode JMA nowcast PNG tiles into precipitation levels.
//!
//! JMA serves 256 x 256, 4-bit indexed PNG tiles. The palette index is
//! significant: indices 0 and 1 both use transparent white, but 0 means "no
//! data" and 1 means "no precipitation". Indices 2..=9 are matched by colour so
//! a reordered palette still decodes correctly. Fully transparent RGBA tiles
//! (served for out-of-domain or odd zoom requests) decode to all zeroes.

use crate::tiles::TILE_SIZE;
use png::{BitDepth, ColorType, Transformations};
use std::fmt;

/// RGB colours for precipitation levels 0..=9.
pub const LEVEL_COLORS: [[u8; 3]; 10] = [
    [255, 255, 255], // 0 no data / transparent
    [255, 255, 255], // 1 no precipitation / transparent
    [242, 242, 255], // 2 [0.25, 1)
    [160, 210, 255], // 3 [1, 5)
    [33, 140, 255],  // 4 [5, 10)
    [0, 65, 255],    // 5 [10, 20)
    [250, 245, 0],   // 6 [20, 30)
    [255, 153, 0],   // 7 [30, 50)
    [255, 40, 0],    // 8 [50, 80)
    [180, 0, 104],   // 9 >= 80
];

const NO_DATA_COLOR: [u8; 3] = LEVEL_COLORS[0];

/// Errors returned while decoding a tile.
#[derive(Debug)]
pub enum DecodeError {
    /// The PNG container or image data is invalid.
    Png(png::DecodingError),
    /// The image is not exactly 256 x 256.
    InvalidSize { width: u32, height: u32 },
    /// The PNG colour type is not indexed, RGB or RGBA.
    UnsupportedColorType(ColorType),
    /// The bit depth is not supported for the colour type.
    UnsupportedBitDepth(ColorType, BitDepth),
    /// An indexed image has no palette.
    MissingPalette,
    /// An indexed image uses an index outside its palette.
    PaletteIndexOutOfRange(u8),
    /// A palette entry that is actually used has an unknown colour.
    UnknownPaletteColor { index: usize, color: [u8; 3] },
    /// An RGB/RGBA pixel that is not transparent has an unknown colour.
    UnknownColor([u8; 3]),
    /// The decoded PNG buffer is shorter than its declared image size.
    Truncated,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Png(err) => write!(f, "invalid PNG tile: {err}"),
            Self::InvalidSize { width, height } => {
                write!(f, "expected a {TILE_SIZE}x{TILE_SIZE} tile, got {width}x{height}")
            }
            Self::UnsupportedColorType(color_type) => {
                write!(f, "unsupported PNG colour type {color_type:?}")
            }
            Self::UnsupportedBitDepth(color_type, bit_depth) => write!(
                f,
                "unsupported bit depth {bit_depth:?} for PNG colour type {color_type:?}"
            ),
            Self::MissingPalette => write!(f, "indexed PNG tile has no palette"),
            Self::PaletteIndexOutOfRange(index) => {
                write!(f, "tile uses palette index {index} outside its palette")
            }
            Self::UnknownPaletteColor { index, color } => write!(
                f,
                "unknown JMA palette colour {color:?} at index {index}; the tile palette may have changed"
            ),
            Self::UnknownColor(color) => write!(
                f,
                "unknown JMA colour {color:?} in RGB/RGBA tile; the palette may have changed"
            ),
            Self::Truncated => write!(f, "PNG tile data is truncated"),
        }
    }
}

impl std::error::Error for DecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Png(err) => Some(err),
            _ => None,
        }
    }
}

impl From<png::DecodingError> for DecodeError {
    fn from(value: png::DecodingError) -> Self {
        Self::Png(value)
    }
}

/// Alias for [`DecodeError`].
pub type Error = DecodeError;

/// Decode a JMA tile PNG into 256 x 256 precipitation levels.
///
/// The returned vector is row-major (`row * 256 + column`) with one byte per
/// pixel. Level 0 means no data, level 1 means no precipitation, and levels
/// 2..=9 are the precipitation intensity classes.
pub fn decode_tile(data: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let mut decoder = png::Decoder::new(data);
    // Keep the palette indices and 4-bit packing intact. Expanding to RGB(A)
    // would lose the distinction between indices 0 and 1, which share a colour.
    decoder.set_transformations(Transformations::IDENTITY);
    let mut reader = decoder.read_info()?;

    let info = reader.info();
    let width = info.width;
    let height = info.height;
    let color_type = info.color_type;
    let bit_depth = info.bit_depth;
    let palette = info.palette.as_ref().map(|p| p.to_vec());

    if width != TILE_SIZE || height != TILE_SIZE {
        return Err(DecodeError::InvalidSize { width, height });
    }

    let mut buffer = vec![0_u8; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buffer)?;
    let buffer = &buffer[..frame.buffer_size()];

    match color_type {
        ColorType::Indexed => decode_indexed(buffer, frame.line_size, bit_depth, palette),
        ColorType::Rgb | ColorType::Rgba => {
            decode_truecolor(buffer, frame.line_size, color_type, bit_depth)
        }
        other => Err(DecodeError::UnsupportedColorType(other)),
    }
}

fn decode_indexed(
    buffer: &[u8],
    line_size: usize,
    bit_depth: BitDepth,
    palette: Option<Vec<u8>>,
) -> Result<Vec<u8>, DecodeError> {
    let bits = match bit_depth {
        BitDepth::One => 1,
        BitDepth::Two => 2,
        BitDepth::Four => 4,
        BitDepth::Eight => 8,
        BitDepth::Sixteen => {
            return Err(DecodeError::UnsupportedBitDepth(
                ColorType::Indexed,
                bit_depth,
            ))
        }
    };

    let palette = palette.ok_or(DecodeError::MissingPalette)?;
    let entries = palette.len() / 3;
    if entries == 0 {
        return Err(DecodeError::MissingPalette);
    }

    let side = TILE_SIZE as usize;
    let mut indices = vec![0_u8; side * side];
    let mut used = [false; 256];

    for y in 0..side {
        let row_start = y * line_size;
        let row = buffer
            .get(row_start..row_start + line_size)
            .ok_or(DecodeError::Truncated)?;
        for x in 0..side {
            let index = packed_sample(row, x, bits).ok_or(DecodeError::Truncated)?;
            if index as usize >= entries {
                return Err(DecodeError::PaletteIndexOutOfRange(index));
            }
            used[index as usize] = true;
            indices[y * side + x] = index;
        }
    }

    let mut lut = [0_u8; 256];
    for index in 0..entries {
        let color = [
            palette[index * 3],
            palette[index * 3 + 1],
            palette[index * 3 + 2],
        ];

        // Indices 0 and 1 share transparent white and are resolved by position.
        if index < 2 && color == NO_DATA_COLOR {
            lut[index] = index as u8;
            continue;
        }

        if let Some(level) = level_for_color(color) {
            lut[index] = level;
            continue;
        }

        if used[index] {
            return Err(DecodeError::UnknownPaletteColor { index, color });
        }
        // An unused padding entry may contain any colour; ignore it.
    }

    Ok(indices
        .into_iter()
        .map(|index| lut[index as usize])
        .collect())
}

fn packed_sample(row: &[u8], x: usize, bits: usize) -> Option<u8> {
    match bits {
        8 => row.get(x).copied(),
        1 | 2 | 4 => {
            let per_byte = 8 / bits;
            let byte = *row.get(x / per_byte)?;
            let slot = x % per_byte;
            let shift = 8 - bits * (slot + 1);
            let mask = (1_u8 << bits) - 1;
            Some((byte >> shift) & mask)
        }
        _ => None,
    }
}

fn decode_truecolor(
    buffer: &[u8],
    line_size: usize,
    color_type: ColorType,
    bit_depth: BitDepth,
) -> Result<Vec<u8>, DecodeError> {
    if bit_depth != BitDepth::Eight {
        return Err(DecodeError::UnsupportedBitDepth(color_type, bit_depth));
    }

    let channels = match color_type {
        ColorType::Rgb => 3,
        ColorType::Rgba => 4,
        other => return Err(DecodeError::UnsupportedColorType(other)),
    };

    let side = TILE_SIZE as usize;
    let mut levels = vec![0_u8; side * side];

    for y in 0..side {
        let row_start = y * line_size;
        let row = buffer
            .get(row_start..row_start + line_size)
            .ok_or(DecodeError::Truncated)?;
        for x in 0..side {
            let offset = x * channels;
            let color = [
                *row.get(offset).ok_or(DecodeError::Truncated)?,
                *row.get(offset + 1).ok_or(DecodeError::Truncated)?,
                *row.get(offset + 2).ok_or(DecodeError::Truncated)?,
            ];

            if channels == 4 && row[offset + 3] == 0 {
                // Transparent pixels are missing data regardless of their RGB.
                continue;
            }

            let level = level_for_color(color).ok_or(DecodeError::UnknownColor(color))?;
            levels[y * side + x] = level;
        }
    }

    Ok(levels)
}

fn level_for_color(color: [u8; 3]) -> Option<u8> {
    LEVEL_COLORS[2..]
        .iter()
        .position(|known| *known == color)
        .map(|index| (index + 2) as u8)
}
