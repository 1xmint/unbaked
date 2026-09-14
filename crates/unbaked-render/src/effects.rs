//! Effects on a layer's own pixels (SPEC.md section 5.5).

use unbaked_core::recipe::{Blend, Color, Effect};

use crate::draw::{bilinear, composite};
use crate::image::{Pixmap, TooLarge, premultiply};
use crate::motion::value_at;

/// An image after effects, and where the original image's top-left pixel sits
/// inside it. Blur and shadow grow the image; the layer box does not move.
#[derive(Debug, Clone, PartialEq)]
pub struct Effected {
    pub image: Pixmap,
    pub origin_x: i64,
    pub origin_y: i64,
}

/// Applies `effects` in order at local time `t_ms`.
pub fn apply(
    image: Pixmap,
    effects: &[Effect],
    t_ms: f64,
    max_pixels: u64,
) -> Result<Effected, TooLarge> {
    let mut out = Effected {
        image,
        origin_x: 0,
        origin_y: 0,
    };
    for effect in effects {
        match effect {
            Effect::Blur { sigma } => {
                let sigma = value_at(sigma, t_ms);
                let (image, radius) = blur(&out.image, sigma, max_pixels)?;
                out.image = image;
                out.origin_x += radius;
                out.origin_y += radius;
            }
            Effect::Shadow {
                dx,
                dy,
                sigma,
                color,
            } => {
                let (image, ox, oy) = shadow(
                    &out.image,
                    value_at(dx, t_ms),
                    value_at(dy, t_ms),
                    value_at(sigma, t_ms),
                    *color,
                    max_pixels,
                )?;
                out.image = image;
                out.origin_x += ox;
                out.origin_y += oy;
            }
            Effect::Adjust {
                brightness,
                contrast,
                saturation,
            } => adjust(
                &mut out.image,
                value_at(brightness, t_ms) as f32,
                value_at(contrast, t_ms) as f32,
                value_at(saturation, t_ms) as f32,
            ),
        }
    }
    Ok(out)
}

fn blank(width: u64, height: u64, max_pixels: u64) -> Result<Pixmap, TooLarge> {
    let w = u32::try_from(width).unwrap_or(u32::MAX);
    let h = u32::try_from(height).unwrap_or(u32::MAX);
    Pixmap::filled(w, h, [0.0; 4], max_pixels)
}

/// Gaussian weights for offsets `-r..=r`, `r = ceil(3σ)`, summing to 1.
fn kernel(sigma: f64) -> Vec<f32> {
    if sigma <= 0.0 || !sigma.is_finite() {
        return vec![1.0];
    }
    let radius = (3.0 * sigma).ceil() as i64;
    let weights: Vec<f64> = (-radius..=radius)
        .map(|i| (-((i * i) as f64) / (2.0 * sigma * sigma)).exp())
        .collect();
    let sum: f64 = weights.iter().sum();
    weights.into_iter().map(|w| (w / sum) as f32).collect()
}

/// Blurs premultiplied colour, growing the image by the kernel radius on every
/// side. Returns the image and the radius.
pub fn blur(image: &Pixmap, sigma: f64, max_pixels: u64) -> Result<(Pixmap, i64), TooLarge> {
    let weights = kernel(sigma);
    let radius = (weights.len() / 2) as i64;
    let (w, h) = (i64::from(image.width), i64::from(image.height));
    let (gw, gh) = (w + 2 * radius, h + 2 * radius);
    let mut across = blank(gw as u64, gh as u64, max_pixels)?;
    let mut out = blank(gw as u64, gh as u64, max_pixels)?;

    // Horizontal pass: grown pixel (x, y) is original pixel (x - r, y - r).
    for y in 0..gh {
        for x in 0..gw {
            let mut sum = [0.0f32; 4];
            for (k, weight) in weights.iter().enumerate() {
                let p = image.pixel_or_clear(x - radius + k as i64 - radius, y - radius);
                for c in 0..4 {
                    sum[c] += p[c] * weight;
                }
            }
            across.set(x as u32, y as u32, sum);
        }
    }
    // Vertical pass.
    for y in 0..gh {
        for x in 0..gw {
            let mut sum = [0.0f32; 4];
            for (k, weight) in weights.iter().enumerate() {
                let p = across.pixel_or_clear(x, y + k as i64 - radius);
                for c in 0..4 {
                    sum[c] += p[c] * weight;
                }
            }
            out.set(x as u32, y as u32, sum);
        }
    }
    Ok((out, radius))
}

/// Draws the image over a blurred, offset copy of its alpha filled with
/// `color`. Returns the grown image and where the original sits in it.
pub fn shadow(
    image: &Pixmap,
    dx: f64,
    dy: f64,
    sigma: f64,
    color: Color,
    max_pixels: u64,
) -> Result<(Pixmap, i64, i64), TooLarge> {
    let fill = [color.r, color.g, color.b, color.a].map(|c| f32::from(c) / 255.0);
    let mut silhouette = image.clone();
    for px in silhouette.data.as_chunks_mut::<4>().0 {
        *px = premultiply([fill[0], fill[1], fill[2], fill[3] * px[3]]);
    }
    let (blurred, radius) = blur(&silhouette, sigma, max_pixels)?;

    let (w, h) = (f64::from(image.width), f64::from(image.height));
    let r = radius as f64;
    let min_x = (dx - r).min(0.0).floor();
    let min_y = (dy - r).min(0.0).floor();
    let max_x = (w + dx + r).max(w).ceil();
    let max_y = (h + dy + r).max(h).ceil();
    let (ox, oy) = (-min_x as i64, -min_y as i64);
    let mut out = blank((max_x - min_x) as u64, (max_y - min_y) as u64, max_pixels)?;

    for y in 0..out.height {
        for x in 0..out.width {
            // Pixel centre in the original image's coordinates.
            let px = f64::from(x) - ox as f64 + 0.5;
            let py = f64::from(y) - oy as f64 + 0.5;
            let under = bilinear(&blurred, px - dx + r, py - dy + r);
            let over = image.pixel_or_clear(i64::from(x) - ox, i64::from(y) - oy);
            out.set(x, y, composite(Blend::Normal, under, over));
        }
    }
    Ok((out, ox, oy))
}

/// Brightness, contrast and saturation on un-premultiplied colour, in that
/// order, clamped after each step (the SVG forms of the CSS filter functions).
pub fn adjust(image: &mut Pixmap, brightness: f32, contrast: f32, saturation: f32) {
    let s = saturation;
    for px in image.data.as_chunks_mut::<4>().0 {
        let a = px[3];
        if a <= 0.0 {
            continue;
        }
        let mut c = [px[0] / a, px[1] / a, px[2] / a];
        c = c.map(|v| (v * brightness).clamp(0.0, 1.0));
        c = c.map(|v| (v * contrast + 0.5 - 0.5 * contrast).clamp(0.0, 1.0));
        let [r, g, b] = c;
        c = [
            (0.213 + 0.787 * s) * r + (0.715 - 0.715 * s) * g + (0.072 - 0.072 * s) * b,
            (0.213 - 0.213 * s) * r + (0.715 + 0.285 * s) * g + (0.072 - 0.072 * s) * b,
            (0.213 - 0.213 * s) * r + (0.715 - 0.715 * s) * g + (0.072 + 0.928 * s) * b,
        ]
        .map(|v| v.clamp(0.0, 1.0));
        *px = [c[0] * a, c[1] * a, c[2] * a, a];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dot(size: u32, x: u32, y: u32) -> Pixmap {
        let mut image = Pixmap::filled(size, size, [0.0; 4], 10_000).unwrap();
        image.set(x, y, [1.0, 1.0, 1.0, 1.0]);
        image
    }

    #[test]
    fn kernel_is_normalised_with_radius_3_sigma() {
        let k = kernel(1.0);
        assert_eq!(k.len(), 7);
        assert!((k.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        assert!((k[3] - 0.39905).abs() < 1e-4, "{}", k[3]);
        assert_eq!(kernel(0.5).len(), 5);
        assert_eq!(kernel(0.0), [1.0]);
    }

    #[test]
    fn blur_grows_the_image_and_keeps_its_total() {
        let (out, radius) = blur(&dot(1, 0, 0), 1.0, 10_000).unwrap();
        assert_eq!(radius, 3);
        assert_eq!((out.width, out.height), (7, 7));
        let total: f32 = out.data.as_chunks::<4>().0.iter().map(|p| p[3]).sum();
        assert!((total - 1.0).abs() < 1e-5, "{total}");
        // The centre stays at the original pixel: weight at 0, squared.
        assert!((out.pixel(3, 3)[3] - 0.39905f32.powi(2)).abs() < 1e-4);
        assert!(out.pixel(0, 3)[3] > 0.0, "spreads out to the full radius");
    }

    #[test]
    fn shadow_sits_under_the_offset_and_grows_to_fit() {
        let black = Color {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        let (out, ox, oy) = shadow(&dot(1, 0, 0), 2.0, 0.0, 0.0, black, 100).unwrap();
        assert_eq!((out.width, out.height, ox, oy), (3, 1, 0, 0));
        assert_eq!(out.pixel(0, 0), [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(out.pixel(1, 0), [0.0; 4]);
        assert_eq!(out.pixel(2, 0), [0.0, 0.0, 0.0, 1.0]);

        // A shadow up and to the left moves the original's position in the image.
        let (out, ox, oy) = shadow(&dot(1, 0, 0), -1.0, -1.0, 0.0, black, 100).unwrap();
        assert_eq!((out.width, out.height, ox, oy), (2, 2, 1, 1));
        assert_eq!(out.pixel(1, 1), [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(out.pixel(0, 0), [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn adjust_follows_the_svg_filter_formulas() {
        let grey = |v: f32| Pixmap {
            width: 1,
            height: 1,
            data: vec![v, v, v, 1.0],
        };
        let mut bright = grey(0.5);
        adjust(&mut bright, 2.0, 1.0, 1.0);
        assert_eq!(bright.data, [1.0, 1.0, 1.0, 1.0]);

        let mut flat = grey(0.9);
        adjust(&mut flat, 1.0, 0.0, 1.0);
        assert_eq!(flat.data, [0.5, 0.5, 0.5, 1.0]);

        let mut red = Pixmap {
            width: 1,
            height: 1,
            data: vec![0.5, 0.0, 0.0, 0.5],
        };
        adjust(&mut red, 1.0, 1.0, 0.0);
        for c in &red.data[..3] {
            assert!((c - 0.213 * 0.5).abs() < 1e-6, "{c}");
        }
        assert_eq!(red.data[3], 0.5);
    }
}
