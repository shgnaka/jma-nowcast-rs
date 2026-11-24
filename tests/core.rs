use jma_nowcast_rs::{
    decode::decode_tile,
    levels::precipitation,
    tiles::{locate, pixel_center, Pixel, TileCoord},
};
use serde_json::json;

#[test]
fn xyz_address_and_inverse_pixel_center() {
    let pixel = locate(35.0, 135.0, 10).unwrap();
    assert_eq!(
        pixel,
        Pixel {
            tile: TileCoord {
                z: 10,
                x: 896,
                y: 405
            },
            u: 0,
            v: 154,
        }
    );
    let (lat, lon) = pixel_center(pixel).unwrap();
    assert!((lat - 35.0).abs() < 0.002);
    assert!((lon - 135.0).abs() < 0.001);
    assert_eq!(locate(lat, lon, 10).unwrap(), pixel);
}

#[test]
fn xyz_rejects_invalid_coordinates_and_zoom_and_keeps_world_edges() {
    let edge = 85.051_128_779_806_6;
    let nw = locate(edge, -180.0, 4).unwrap();
    let se = locate(-edge, 180.0, 4).unwrap();
    assert_eq!((nw.tile.x, nw.tile.y, nw.u, nw.v), (0, 0, 0, 0));
    assert_eq!((se.tile.x, se.tile.y, se.u, se.v), (15, 15, 255, 255));
    assert!(locate(90.0, 0.0, 10).is_err());
    assert!(locate(0.0, -181.0, 10).is_err());
    assert!(locate(f64::NAN, 0.0, 10).is_err());
    assert!(locate(0.0, f64::INFINITY, 10).is_err());
    assert!(locate(0.0, 0.0, 9).is_err());
}

#[test]
fn every_jma_pixel_matches_python_reference() {
    let levels = decode_tile(include_bytes!("fixtures/20260916010500_z6_x56_y25.png")).unwrap();
    let expected = include_bytes!("fixtures/20260916010500_z6_x56_y25.levels");
    assert_eq!(
        levels.as_slice(),
        expected,
        "decoded pixel order or values changed"
    );
    let mut histogram = [0; 10];
    for level in levels {
        histogram[usize::from(level)] += 1;
    }
    assert_eq!(
        histogram,
        [13195, 16449, 7812, 16467, 5333, 4668, 1014, 420, 152, 26]
    );
}

#[test]
fn transparent_rgba_is_entirely_missing() {
    let levels = decode_tile(include_bytes!(
        "fixtures/20260916010500_z4_x14_y7_empty_rgba.png"
    ))
    .unwrap();
    assert_eq!(levels, vec![0; 256 * 256]);
}

fn packed_tile(width: u32, first_byte: u8) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, 256);
        encoder.set_color(png::ColorType::Indexed);
        encoder.set_depth(png::BitDepth::Four);
        // Entries 2 and 3 are deliberately reordered. Entry 4 is an unknown color.
        encoder.set_palette(vec![
            255, 255, 255, 255, 255, 255, 180, 0, 104, 242, 242, 255, 1, 2, 3,
        ]);
        encoder.set_trns(vec![0, 0, 255, 255, 255]);
        let mut pixels = vec![0x01; ((width + 1) / 2) as usize * 256];
        pixels[0] = first_byte;
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&pixels)
            .unwrap();
    }
    bytes
}

#[test]
fn packed_indices_preserve_missing_and_dry_and_follow_colors() {
    let levels = decode_tile(&packed_tile(256, 0x23)).unwrap();
    assert_eq!(levels.len(), 256 * 256);
    assert_eq!(&levels[..4], &[9, 2, 0, 1]);
    assert_eq!(&levels[254..258], &[0, 1, 0, 1]);
}

#[test]
fn invalid_tile_content_is_rejected() {
    assert!(decode_tile(&packed_tile(256, 0x40)).is_err());
    assert!(decode_tile(&packed_tile(256, 0x50)).is_err());
    assert!(decode_tile(&packed_tile(255, 0x01)).is_err());
    assert!(decode_tile(b"this is not a PNG").is_err());
    assert!(decode_tile(&packed_tile(256, 0x01)[..42]).is_err());
}

#[test]
fn precipitation_serializes_status_and_all_bounds() {
    let bounds = [0.0, 0.25, 1.0, 5.0, 10.0, 20.0, 30.0, 50.0, 80.0];
    for level in 0_u8..=9 {
        let expected_status = match level {
            0 => "no_data",
            1 => "no_rain",
            _ => "rain",
        };
        let actual = serde_json::to_value(precipitation(level).unwrap()).unwrap();
        assert_eq!(actual["level"], json!(level));
        assert_eq!(actual["status"], json!(expected_status));
        assert_eq!(
            actual["min_mm_h"],
            json!(level.checked_sub(1).map(|i| bounds[usize::from(i)]))
        );
        assert_eq!(
            actual["max_mm_h"],
            json!((level > 0)
                .then(|| bounds.get(usize::from(level)).copied())
                .flatten())
        );
        assert!(actual.get("min_mm_h").is_some());
        assert!(actual.get("max_mm_h").is_some());
    }
    assert!(precipitation(10).is_err());
}

#[test]
fn rgb_and_rgba_decode_known_colors_and_reject_unknown_opaque_colors() {
    for (color, channels) in [(png::ColorType::Rgb, 3), (png::ColorType::Rgba, 4)] {
        let mut pixels = [33, 140, 255, 255][..channels].repeat(256 * 256);
        if channels == 4 {
            pixels[3] = 0;
        }
        let encode = |pixels: &[u8]| {
            let mut data = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut data, 256, 256);
                encoder.set_color(color);
                encoder.set_depth(png::BitDepth::Eight);
                encoder
                    .write_header()
                    .unwrap()
                    .write_image_data(pixels)
                    .unwrap();
            }
            data
        };
        let levels = decode_tile(&encode(&pixels)).unwrap();
        assert_eq!(levels[0], if channels == 4 { 0 } else { 4 });
        assert!(levels[1..].iter().all(|&level| level == 4));
        pixels[channels..channels + 3].copy_from_slice(&[1, 2, 3]);
        assert!(decode_tile(&encode(&pixels)).is_err());
    }
}

#[test]
fn png_truncated_after_pixels_is_rejected() {
    let png = packed_tile(256, 0x01);
    assert!(decode_tile(&png[..png.len() - 12]).is_err(), "missing IEND");
}

#[test]
fn indexed_palette_cannot_exceed_bit_depth_capacity() {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 256, 256);
        encoder.set_color(png::ColorType::Indexed);
        encoder.set_depth(png::BitDepth::Four);
        // Four bits can address exactly 16 entries, even when the last entry is unused.
        encoder.set_palette([255, 255, 255].repeat(17));
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&vec![0; 128 * 256])
            .unwrap();
    }
    assert!(decode_tile(&bytes).is_err());
}
