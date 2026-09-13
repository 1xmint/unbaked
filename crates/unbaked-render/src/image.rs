//! Pixel buffers, image decoding and PNG output (SPEC.md sections 5.1 and 5.7).

use std::io::Cursor;

use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

/// Premultiplied RGBA pixels, 0–1, gamma-encoded sRGB, row by row from the top.
#[derive(Debug, Clone, PartialEq)]
pub struct Pixmap {
    pub width: u32,
    pub height: u32,
    /// `width × height × 4` values.
    pub data: Vec<f32>,
}

/// Why a buffer could not be made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TooLarge {
    pub pixels: u64,
    pub limit: u64,
}

impl Pixmap {
    /// A buffer filled with one premultiplied colour, refused past `max_pixels`.
    pub fn filled(
        width: u32,
        height: u32,
        rgba: [f32; 4],
        max_pixels: u64,
    ) -> Result<Pixmap, TooLarge> {
        let pixels = u64::from(width) * u64::from(height);
        if pixels > max_pixels {
            return Err(TooLarge {
                pixels,
                limit: max_pixels,
            });
        }
        let mut data = Vec::with_capacity(pixels as usize * 4);
        for _ in 0..pixels {
            data.extend_from_slice(&rgba);
        }
        Ok(Pixmap {
            width,
            height,
            data,
        })
    }

    pub fn pixel(&self, x: u32, y: u32) -> [f32; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.data[i],
            self.data[i + 1],
            self.data[i + 2],
            self.data[i + 3],
        ]
    }

    /// A pixel, or transparent outside the buffer.
    pub fn pixel_or_clear(&self, x: i64, y: i64) -> [f32; 4] {
        if x < 0 || y < 0 || x >= i64::from(self.width) || y >= i64::from(self.height) {
            [0.0; 4]
        } else {
            self.pixel(x as u32, y as u32)
        }
    }

    pub fn set(&mut self, x: u32, y: u32, rgba: [f32; 4]) {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        self.data[i..i + 4].copy_from_slice(&rgba);
    }

    /// Final output (section 5.7): un-premultiply, `round(v × 255)` with halves
    /// up, clamp. A pixel with alpha 0 becomes all zeros.
    pub fn to_rgba8(&self) -> Vec<u8> {
        let quantise = |v: f32| (v * 255.0 + 0.5).floor().clamp(0.0, 255.0) as u8;
        let mut out = Vec::with_capacity(self.data.len());
        for px in self.data.chunks_exact(4) {
            let a = px[3];
            if a <= 0.0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                out.extend_from_slice(&[
                    quantise(px[0] / a),
                    quantise(px[1] / a),
                    quantise(px[2] / a),
                    quantise(a),
                ]);
            }
        }
        out
    }
}

/// Premultiplies straight colour.
pub fn premultiply([r, g, b, a]: [f32; 4]) -> [f32; 4] {
    [r * a, g * a, b * a, a]
}

fn from_samples(
    width: u32,
    height: u32,
    channels: usize,
    samples: impl Iterator<Item = f32>,
) -> Pixmap {
    let mut data = Vec::with_capacity(width as usize * height as usize * 4);
    let mut px = [0.0f32; 4];
    for (i, v) in samples.enumerate() {
        px[i % channels] = v;
        if i % channels == channels - 1 {
            let rgba = match channels {
                1 => [px[0], px[0], px[0], 1.0],
                2 => [px[0], px[0], px[0], px[1]],
                3 => [px[0], px[1], px[2], 1.0],
                _ => px,
            };
            data.extend_from_slice(&premultiply(rgba));
        }
    }
    Pixmap {
        width,
        height,
        data,
    }
}

/// Decodes a PNG. Colour profiles and gamma chunks are ignored (section 5.1).
pub fn decode_png(bytes: &[u8], max_pixels: u64) -> Result<Pixmap, String> {
    let limits = png::Limits {
        bytes: usize::try_from(max_pixels.saturating_mul(8)).unwrap_or(usize::MAX),
    };
    let mut decoder = png::Decoder::new_with_limits(Cursor::new(bytes), limits);
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let (width, height) = (reader.info().width, reader.info().height);
    let pixels = u64::from(width) * u64::from(height);
    if pixels > max_pixels {
        return Err(format!("{width}x{height} is more than {max_pixels} pixels"));
    }
    let size = reader
        .output_buffer_size()
        .ok_or("image is too large to decode")?;
    let mut buf = vec![0; size];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    buf.truncate(info.buffer_size());
    let channels = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return Err("palette was not expanded".into()),
    };
    Ok(match info.bit_depth {
        png::BitDepth::Sixteen => from_samples(
            width,
            height,
            channels,
            buf.chunks_exact(2)
                .map(|s| f32::from(u16::from_be_bytes([s[0], s[1]])) / 65535.0),
        ),
        _ => from_samples(
            width,
            height,
            channels,
            buf.iter().map(|&s| f32::from(s) / 255.0),
        ),
    })
}

/// Decodes a JPEG and turns it upright by its EXIF orientation (section 5.1).
pub fn decode_jpeg(bytes: &[u8], max_pixels: u64) -> Result<Pixmap, String> {
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut decoder = JpegDecoder::new_with_options(Cursor::new(bytes), options);
    decoder.decode_headers().map_err(|e| e.to_string())?;
    let info = decoder.info().ok_or("no JPEG header")?;
    let (width, height) = (u32::from(info.width), u32::from(info.height));
    let pixels = u64::from(width) * u64::from(height);
    if pixels > max_pixels {
        return Err(format!("{width}x{height} is more than {max_pixels} pixels"));
    }
    let orientation = decoder.exif().map_or(1, |exif| exif_orientation(exif));
    let rgb = decoder.decode().map_err(|e| e.to_string())?;
    let image = from_samples(width, height, 3, rgb.iter().map(|&s| f32::from(s) / 255.0));
    Ok(orient(&image, orientation))
}

/// The EXIF orientation tag (1–8) from TIFF-structured EXIF data, or 1.
pub fn exif_orientation(tiff: &[u8]) -> u16 {
    let big = match tiff.get(0..4) {
        Some(b"MM\0*") => true,
        Some(b"II*\0") => false,
        _ => return 1,
    };
    let u16_at = |i: usize| {
        tiff.get(i..i + 2).map(|b| {
            if big {
                u16::from_be_bytes([b[0], b[1]])
            } else {
                u16::from_le_bytes([b[0], b[1]])
            }
        })
    };
    let u32_at = |i: usize| {
        tiff.get(i..i + 4).map(|b| {
            let b = [b[0], b[1], b[2], b[3]];
            if big {
                u32::from_be_bytes(b)
            } else {
                u32::from_le_bytes(b)
            }
        })
    };
    let Some(ifd) = u32_at(4).map(|o| o as usize) else {
        return 1;
    };
    let Some(count) = u16_at(ifd) else {
        return 1;
    };
    for n in 0..usize::from(count) {
        let entry = ifd + 2 + n * 12;
        if u16_at(entry) == Some(0x0112) {
            return u16_at(entry + 8)
                .filter(|o| (1..=8).contains(o))
                .unwrap_or(1);
        }
    }
    1
}

/// Applies an EXIF orientation, returning the image as it should be displayed.
pub fn orient(image: &Pixmap, orientation: u16) -> Pixmap {
    let (w, h) = (image.width, image.height);
    let swaps = (5..=8).contains(&orientation);
    let (out_w, out_h) = if swaps { (h, w) } else { (w, h) };
    let mut out = Pixmap {
        width: out_w,
        height: out_h,
        data: vec![0.0; image.data.len()],
    };
    for y in 0..out_h {
        for x in 0..out_w {
            let (sx, sy) = match orientation {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (y, h - 1 - x),
                7 => (w - 1 - y, h - 1 - x),
                8 => (w - 1 - y, x),
                _ => (x, y),
            };
            out.set(x, y, image.pixel(sx, sy));
        }
    }
    out
}

/// Encodes 8-bit RGBA as a PNG with an `sRGB` chunk (section 5.7).
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgba).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantising_rounds_halves_up_and_clears_transparent_pixels() {
        let image = Pixmap {
            width: 2,
            height: 1,
            data: vec![
                0.5, 0.25, 0.0, 0.5, // straight (1.0, 0.5, 0), alpha 0.5: 127.5 rounds up
                0.3, 0.3, 0.3, 0.0, // alpha 0 becomes all zeros
            ],
        };
        assert_eq!(image.to_rgba8(), [255, 128, 0, 128, 0, 0, 0, 0]);
    }

    #[test]
    fn png_round_trips_including_16_bit_and_grey() {
        let rgba = [255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 0, 0, 10, 20, 30, 40];
        let png = encode_png(2, 2, &rgba).unwrap();
        let image = decode_png(&png, 100).unwrap();
        assert_eq!((image.width, image.height), (2, 2));
        assert_eq!(image.to_rgba8(), rgba);
        assert!(png.windows(4).any(|w| w == b"sRGB"));

        let mut sixteen = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut sixteen, 1, 1);
            encoder.set_color(png::ColorType::GrayscaleAlpha);
            encoder.set_depth(png::BitDepth::Sixteen);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[0x80, 0x00, 0xff, 0xff]).unwrap();
        }
        let grey = decode_png(&sixteen, 100).unwrap();
        let expected = 32768.0 / 65535.0;
        assert!((grey.data[0] - expected).abs() < 1e-6);
        assert_eq!(grey.data[3], 1.0);

        assert!(
            decode_png(&png, 3)
                .unwrap_err()
                .contains("more than 3 pixels")
        );
    }

    fn tiff(big: bool, orientation: u16) -> Vec<u8> {
        let u16b = |v: u16| {
            if big {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            }
        };
        let u32b = |v: u32| {
            if big {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            }
        };
        let mut out = if big {
            b"MM\0*".to_vec()
        } else {
            b"II*\0".to_vec()
        };
        out.extend(u32b(8));
        out.extend(u16b(2));
        // An unrelated tag first, then orientation: tag, SHORT, count 1, value.
        out.extend(u16b(0x010f));
        out.extend(u16b(2));
        out.extend(u32b(4));
        out.extend(u32b(0));
        out.extend(u16b(0x0112));
        out.extend(u16b(3));
        out.extend(u32b(1));
        out.extend(u16b(orientation));
        out.extend([0, 0]);
        out
    }

    #[test]
    fn exif_orientation_is_read_in_both_byte_orders() {
        assert_eq!(exif_orientation(&tiff(true, 6)), 6);
        assert_eq!(exif_orientation(&tiff(false, 8)), 8);
        assert_eq!(exif_orientation(&tiff(false, 9)), 1);
        assert_eq!(exif_orientation(b"junk"), 1);
        assert_eq!(exif_orientation(&tiff(true, 3)[..20]), 1);
    }

    #[test]
    fn orientations_turn_the_image_upright() {
        // A 2x1 image: red then green, as stored.
        let red = [1.0, 0.0, 0.0, 1.0];
        let green = [0.0, 1.0, 0.0, 1.0];
        let image = Pixmap {
            width: 2,
            height: 1,
            data: [red, green].concat(),
        };
        let column = |o| {
            let out = orient(&image, o);
            assert_eq!((out.width, out.height), (1, 2));
            (out.pixel(0, 0), out.pixel(0, 1))
        };
        // 6: rotate 90° clockwise, so the left pixel ends on top.
        assert_eq!(column(6), (red, green));
        // 8: rotate 90° anticlockwise, so the right pixel ends on top.
        assert_eq!(column(8), (green, red));
        let flipped = orient(&image, 2);
        assert_eq!((flipped.pixel(0, 0), flipped.pixel(1, 0)), (green, red));
        assert_eq!(orient(&image, 1), image);
    }
}
