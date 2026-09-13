//! Placing a layer's pixels on the canvas: transforms, sampling and blending
//! (SPEC.md sections 5.3 and 5.4).

use unbaked_core::recipe::Blend;

use crate::image::Pixmap;

/// An affine map `(x, y) -> (a·x + c·y + e, b·x + d·y + f)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Affine {
    pub const IDENTITY: Affine = Affine {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    pub fn translate(x: f64, y: f64) -> Affine {
        Affine {
            e: x,
            f: y,
            ..Affine::IDENTITY
        }
    }

    pub fn scale(sx: f64, sy: f64) -> Affine {
        Affine {
            a: sx,
            d: sy,
            ..Affine::IDENTITY
        }
    }

    /// Clockwise on screen, where y points down.
    pub fn rotate_deg(deg: f64) -> Affine {
        let (sin, cos) = deg.to_radians().sin_cos();
        Affine {
            a: cos,
            b: sin,
            c: -sin,
            d: cos,
            ..Affine::IDENTITY
        }
    }

    /// `self` after `inner`: applies `inner` first.
    pub fn then_after(self, inner: Affine) -> Affine {
        Affine {
            a: self.a * inner.a + self.c * inner.b,
            b: self.b * inner.a + self.d * inner.b,
            c: self.a * inner.c + self.c * inner.d,
            d: self.b * inner.c + self.d * inner.d,
            e: self.a * inner.e + self.c * inner.f + self.e,
            f: self.b * inner.e + self.d * inner.f + self.f,
        }
    }

    pub fn apply(self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    /// The inverse map, or `None` when the layer collapses to nothing (a scale of 0).
    pub fn invert(self) -> Option<Affine> {
        let det = self.a * self.d - self.b * self.c;
        if det.abs() < 1e-12 || !det.is_finite() {
            return None;
        }
        Some(Affine {
            a: self.d / det,
            b: -self.b / det,
            c: -self.c / det,
            d: self.a / det,
            e: (self.c * self.f - self.d * self.e) / det,
            f: (self.b * self.e - self.a * self.f) / det,
        })
    }

    /// The shorter of the two column lengths: how many canvas pixels one
    /// source pixel covers along its narrower axis.
    pub fn min_stretch(self) -> f64 {
        self.a.hypot(self.b).min(self.c.hypot(self.d))
    }
}

/// Samples bilinearly at source coordinates `(u, v)`. Pixel centres sit at
/// `+0.5`; outside the image is transparent.
pub fn bilinear(image: &Pixmap, u: f64, v: f64) -> [f32; 4] {
    let (fx, fy) = (u - 0.5, v - 0.5);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = ((fx - x0) as f32, (fy - y0) as f32);
    let (x0, y0) = (x0 as i64, y0 as i64);
    let p00 = image.pixel_or_clear(x0, y0);
    let p10 = image.pixel_or_clear(x0 + 1, y0);
    let p01 = image.pixel_or_clear(x0, y0 + 1);
    let p11 = image.pixel_or_clear(x0 + 1, y0 + 1);
    let mut out = [0.0; 4];
    for i in 0..4 {
        let top = p00[i] + (p10[i] - p00[i]) * tx;
        let bottom = p01[i] + (p11[i] - p01[i]) * tx;
        out[i] = top + (bottom - top) * ty;
    }
    out
}

/// The next mip level: half the size, rounding up, each pixel the average of
/// the up-to-4 pixels it covers.
pub fn half(image: &Pixmap) -> Pixmap {
    let (w, h) = (image.width.div_ceil(2), image.height.div_ceil(2));
    let mut out = Pixmap {
        width: w,
        height: h,
        data: vec![0.0; w as usize * h as usize * 4],
    };
    for y in 0..h {
        for x in 0..w {
            let mut sum = [0.0f32; 4];
            let mut count = 0.0;
            for (sx, sy) in [
                (2 * x, 2 * y),
                (2 * x + 1, 2 * y),
                (2 * x, 2 * y + 1),
                (2 * x + 1, 2 * y + 1),
            ] {
                if sx < image.width && sy < image.height {
                    let p = image.pixel(sx, sy);
                    for i in 0..4 {
                        sum[i] += p[i];
                    }
                    count += 1.0;
                }
            }
            out.set(x, y, sum.map(|v| v / count));
        }
    }
    out
}

/// The separable blend function `B(cb, cs)` of W3C Compositing and Blending
/// Level 1, on straight colour values.
pub fn blend_channel(mode: Blend, cb: f32, cs: f32) -> f32 {
    let multiply = |b: f32, s: f32| b * s;
    let screen = |b: f32, s: f32| b + s - b * s;
    let hard_light = |b: f32, s: f32| {
        if s <= 0.5 {
            multiply(b, 2.0 * s)
        } else {
            screen(b, 2.0 * s - 1.0)
        }
    };
    match mode {
        Blend::Normal => cs,
        Blend::Multiply => multiply(cb, cs),
        Blend::Screen => screen(cb, cs),
        Blend::Overlay => hard_light(cs, cb),
        Blend::Darken => cb.min(cs),
        Blend::Lighten => cb.max(cs),
        Blend::ColorDodge => {
            if cb == 0.0 {
                0.0
            } else if cs >= 1.0 {
                1.0
            } else {
                (cb / (1.0 - cs)).min(1.0)
            }
        }
        Blend::ColorBurn => {
            if cb >= 1.0 {
                1.0
            } else if cs <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - cb) / cs).min(1.0)
            }
        }
        Blend::HardLight => hard_light(cb, cs),
        Blend::SoftLight => {
            if cs <= 0.5 {
                cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
            } else {
                let d = if cb <= 0.25 {
                    ((16.0 * cb - 12.0) * cb + 4.0) * cb
                } else {
                    cb.sqrt()
                };
                cb + (2.0 * cs - 1.0) * (d - cb)
            }
        }
        Blend::Difference => (cb - cs).abs(),
        Blend::Exclusion => cb + cs - 2.0 * cb * cs,
    }
}

/// Composites premultiplied `src` over premultiplied `dst` with `mode`:
/// `co = cs·(1−αb) + cb·(1−αs) + αs·αb·B(Cb, Cs)`, `αo = αs + αb·(1−αs)`.
pub fn composite(mode: Blend, dst: [f32; 4], src: [f32; 4]) -> [f32; 4] {
    let (ab, a_s) = (dst[3], src[3]);
    let mut out = [0.0; 4];
    for i in 0..3 {
        out[i] = if mode == Blend::Normal {
            src[i] + dst[i] * (1.0 - a_s)
        } else {
            let straight_b = if ab > 0.0 { dst[i] / ab } else { 0.0 };
            let straight_s = if a_s > 0.0 { src[i] / a_s } else { 0.0 };
            src[i] * (1.0 - ab)
                + dst[i] * (1.0 - a_s)
                + a_s * ab * blend_channel(mode, straight_b, straight_s)
        };
    }
    out[3] = a_s + ab * (1.0 - a_s);
    out
}

/// Samples `source` through `to_canvas` onto `canvas` (section 5.3), scaled by
/// `opacity`, blended with `mode` (section 5.4). Only pixels the layer can
/// touch are visited.
pub fn place(canvas: &mut Pixmap, source: &Pixmap, to_canvas: Affine, opacity: f32, mode: Blend) {
    let Some(to_source) = to_canvas.invert() else {
        return;
    };
    if opacity <= 0.0 || source.width == 0 || source.height == 0 {
        return;
    }

    let stretch = to_canvas.min_stretch();
    let mut level = source.clone();
    let mut divisor = 1.0;
    if stretch < 1.0 {
        let wanted = (1.0 / stretch).log2().floor() as u32;
        for _ in 0..wanted {
            if level.width == 1 && level.height == 1 {
                break;
            }
            level = half(&level);
            divisor *= 2.0;
        }
    }

    // Bilinear sampling reaches half a pixel past the edge; one pixel of margin covers it.
    let (w, h) = (f64::from(source.width), f64::from(source.height));
    let corners = [
        (-1.0, -1.0),
        (w + 1.0, -1.0),
        (-1.0, h + 1.0),
        (w + 1.0, h + 1.0),
    ];
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (x, y) in corners {
        let (cx, cy) = to_canvas.apply(x, y);
        min_x = min_x.min(cx);
        min_y = min_y.min(cy);
        max_x = max_x.max(cx);
        max_y = max_y.max(cy);
    }
    let clamp = |v: f64, size: u32| v.clamp(0.0, f64::from(size)) as u32;
    let (x0, x1) = (
        clamp(min_x.floor(), canvas.width),
        clamp(max_x.ceil(), canvas.width),
    );
    let (y0, y1) = (
        clamp(min_y.floor(), canvas.height),
        clamp(max_y.ceil(), canvas.height),
    );

    for py in y0..y1 {
        for px in x0..x1 {
            let (u, v) = to_source.apply(f64::from(px) + 0.5, f64::from(py) + 0.5);
            let sample = bilinear(&level, u / divisor, v / divisor);
            if sample[3] <= 0.0 {
                continue;
            }
            let src = sample.map(|c| c * opacity);
            let dst = canvas.pixel(px, py);
            canvas.set(px, py, composite(mode, dst, src));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn affine_compose_and_invert() {
        let m = Affine::translate(10.0, 5.0)
            .then_after(Affine::rotate_deg(90.0))
            .then_after(Affine::scale(2.0, 3.0));
        let (x, y) = m.apply(1.0, 0.0);
        // Scale to (2, 0), rotate clockwise to (0, 2), move to (10, 7).
        assert!((x - 10.0).abs() < 1e-9 && (y - 7.0).abs() < 1e-9);
        let back = m.invert().unwrap().apply(x, y);
        assert!((back.0 - 1.0).abs() < 1e-9 && back.1.abs() < 1e-9);
        assert_eq!(Affine::scale(0.0, 1.0).invert(), None);
        assert!((m.min_stretch() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn bilinear_fades_to_transparent_past_the_edge() {
        let image = Pixmap::filled(2, 2, [1.0, 0.0, 0.0, 1.0], 100).unwrap();
        assert_eq!(bilinear(&image, 1.0, 1.0), [1.0, 0.0, 0.0, 1.0]);
        let edge = bilinear(&image, 0.25, 1.0);
        assert!(close(edge[3], 0.75), "{edge:?}");
        assert_eq!(bilinear(&image, -5.0, 1.0)[3], 0.0);
    }

    #[test]
    fn mip_levels_average_what_they_cover() {
        let mut image = Pixmap::filled(3, 1, [0.0; 4], 100).unwrap();
        image.set(0, 0, [1.0, 1.0, 1.0, 1.0]);
        image.set(2, 0, [0.5, 0.5, 0.5, 0.5]);
        let next = half(&image);
        assert_eq!((next.width, next.height), (2, 1));
        assert_eq!(next.pixel(0, 0), [0.5, 0.5, 0.5, 0.5]);
        // The last column has one pixel, and missing pixels are not counted.
        assert_eq!(next.pixel(1, 0), [0.5, 0.5, 0.5, 0.5]);
    }

    #[test]
    fn blend_modes_match_w3c_formulas() {
        let (cb, cs) = (0.25f32, 0.75f32);
        let cases = [
            (Blend::Normal, 0.75),
            (Blend::Multiply, 0.1875),
            (Blend::Screen, 0.8125),
            (Blend::Overlay, 0.375),
            (Blend::Darken, 0.25),
            (Blend::Lighten, 0.75),
            (Blend::ColorDodge, 1.0),
            (Blend::ColorBurn, 0.0),
            (Blend::HardLight, 0.625),
            (
                Blend::SoftLight,
                0.25 + 0.5 * ((((16.0 * 0.25 - 12.0) * 0.25 + 4.0) * 0.25) - 0.25),
            ),
            (Blend::Difference, 0.5),
            (Blend::Exclusion, 0.625),
        ];
        for (mode, expected) in cases {
            let got = blend_channel(mode, cb, cs);
            assert!(close(got, expected), "{mode:?}: {got} != {expected}");
        }
    }

    #[test]
    fn compositing_on_opaque_and_clear_backdrops() {
        let grey = [0.5, 0.5, 0.5, 1.0];
        let red = [1.0, 0.0, 0.0, 1.0];
        assert_eq!(composite(Blend::Multiply, grey, red), [0.5, 0.0, 0.0, 1.0]);
        // Any mode over a transparent backdrop is just the source.
        assert_eq!(composite(Blend::Difference, [0.0; 4], red), red);
        let half_red = [0.5, 0.0, 0.0, 0.5];
        assert_eq!(
            composite(Blend::Normal, grey, half_red),
            [0.75, 0.25, 0.25, 1.0]
        );
    }

    #[test]
    fn place_draws_an_aligned_box_exactly() {
        let mut canvas = Pixmap::filled(4, 4, [0.0; 4], 100).unwrap();
        let solid = Pixmap::filled(2, 2, [0.0, 0.0, 1.0, 1.0], 100).unwrap();
        place(
            &mut canvas,
            &solid,
            Affine::translate(1.0, 1.0),
            1.0,
            Blend::Normal,
        );
        for y in 0..4 {
            for x in 0..4 {
                let inside = (1..3).contains(&x) && (1..3).contains(&y);
                let expected = if inside {
                    [0.0, 0.0, 1.0, 1.0]
                } else {
                    [0.0; 4]
                };
                assert_eq!(canvas.pixel(x, y), expected, "({x}, {y})");
            }
        }
    }

    #[test]
    fn shrinking_uses_the_mip_chain() {
        // A black and white checkerboard shrunk 8 times is mid grey, not aliased.
        let mut board = Pixmap::filled(8, 8, [0.0, 0.0, 0.0, 1.0], 100).unwrap();
        for y in 0..8 {
            for x in 0..8 {
                if (x + y) % 2 == 0 {
                    board.set(x, y, [1.0, 1.0, 1.0, 1.0]);
                }
            }
        }
        let mut canvas = Pixmap::filled(1, 1, [0.0; 4], 100).unwrap();
        place(
            &mut canvas,
            &board,
            Affine::scale(0.125, 0.125),
            1.0,
            Blend::Normal,
        );
        assert_eq!(canvas.to_rgba8(), [128, 128, 128, 255]);
    }
}
