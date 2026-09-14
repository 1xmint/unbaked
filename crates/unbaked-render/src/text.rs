//! Text layers (SPEC.md sections 4.5, 4.6 and 5.6): line breaking, shaping,
//! layout and glyph filling.

use std::ops::Range;

use harfrust::{Direction, ShapeOptions, Shaper, ShaperData, UnicodeBuffer};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::raw::TableProvider;
use skrifa::{FontRef, GlyphId, MetadataProvider};
use unbaked_core::recipe::{Align, Text};
use unicode_bidi::ParagraphBidiInfo;
use unicode_linebreak::{BreakOpportunity, linebreaks};
use unicode_script::{Script, UnicodeScript};

use crate::image::{Pixmap, TooLarge, premultiply};
use crate::scene::{Deadline, RenderLimits};

/// A shaped glyph. Positions are font units from the start of its line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyph {
    pub id: u32,
    /// Byte offset in the paragraph of the characters it came from.
    pub cluster: u32,
    pub x: i64,
    pub y: i64,
    pub advance: i64,
}

/// One laid-out line.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// Glyphs in visual order, left to right.
    pub glyphs: Vec<Glyph>,
    /// Advance width in font units, without trailing white space.
    pub width: i64,
    /// Left edge within the layer box, in pixels.
    pub x: f64,
    /// Baseline within the layer box, in pixels.
    pub baseline: f64,
}

/// A text layer's lines and layer box.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub lines: Vec<Line>,
    /// Pixels per font unit.
    pub scale: f64,
    pub box_width: f64,
    pub box_height: f64,
}

/// Why text could not be drawn.
#[derive(Debug, Clone, PartialEq)]
pub enum TextError {
    /// The font file is broken or unusable.
    Font(String),
    TooLarge(TooLarge),
    /// [`RenderLimits::deadline`] passed.
    TimedOut {
        limit_ms: u64,
    },
}

/// A text layer drawn into its source image: one pixel per box unit, covering
/// all glyph ink. The layer box's top-left corner sits at pixel
/// `(origin_x, origin_y)` of `image`.
#[derive(Debug, Clone, PartialEq)]
pub struct Drawn {
    pub image: Pixmap,
    pub box_width: f64,
    pub box_height: f64,
    pub origin_x: i64,
    pub origin_y: i64,
}

/// Opens face `index` of a font file.
pub fn font(data: &[u8], index: u32) -> Result<FontRef<'_>, TextError> {
    FontRef::from_index(data, index).map_err(|e| TextError::Font(e.to_string()))
}

/// Lays out `text` with `font`.
pub fn layout(text: &Text, font: &FontRef) -> Result<Layout, TextError> {
    let bad = |e: skrifa::raw::ReadError| TextError::Font(e.to_string());
    let units_per_em = font.head().map_err(bad)?.units_per_em();
    if units_per_em == 0 {
        return Err(TextError::Font("units per em is 0".into()));
    }
    let hhea = font.hhea().map_err(bad)?;
    let ascender = f64::from(hhea.ascender().to_i16()).abs();
    let descender = f64::from(hhea.descender().to_i16()).abs();
    let scale = text.size_px / f64::from(units_per_em);

    let data = ShaperData::new(font);
    let shaper = data.shaper(font).build();
    let mut lines = Vec::new();
    for paragraph in paragraphs(&text.text) {
        let bidi = ParagraphBidiInfo::new(paragraph, None);
        let whole = 0..paragraph.len();
        let ranges = match text.box_width {
            None => vec![whole],
            Some(box_width) => wrap(paragraph, &shaper, &bidi, box_width / scale),
        };
        for range in ranges {
            let end = range.start + paragraph[range.clone()].trim_end().len();
            let (glyphs, width) = shape(&shaper, paragraph, &bidi, range.start..end);
            lines.push(Line {
                glyphs,
                width,
                x: 0.0,
                baseline: 0.0,
            });
        }
    }

    let widest = lines.iter().map(|l| l.width).max().unwrap_or(0);
    let box_width = text.box_width.unwrap_or(widest as f64 * scale);
    let step = text.line_height * text.size_px;
    let first = (step - (ascender + descender) * scale) / 2.0 + ascender * scale;
    for (i, line) in lines.iter_mut().enumerate() {
        let spare = box_width - line.width as f64 * scale;
        line.x = match text.align {
            Align::Left => 0.0,
            Align::Center => spare / 2.0,
            Align::Right => spare,
        };
        line.baseline = first + i as f64 * step;
    }
    Ok(Layout {
        box_height: step * lines.len() as f64,
        lines,
        scale,
        box_width,
    })
}

/// Draws `text` with the font file `data` into its source image.
pub fn draw(text: &Text, data: &[u8], limits: RenderLimits) -> Result<Drawn, TextError> {
    let font = font(data, text.font_index)?;
    let layout = layout(text, &font)?;
    let outlines = font.outline_glyphs();
    let mut pen = Segments {
        scale: layout.scale,
        ..Segments::default()
    };
    for line in &layout.lines {
        if let Some(deadline) = limits.deadline.filter(Deadline::passed) {
            return Err(TextError::TimedOut {
                limit_ms: deadline.limit_ms(),
            });
        }
        for glyph in &line.glyphs {
            let Some(outline) = outlines.get(GlyphId::new(glyph.id)) else {
                continue;
            };
            pen.origin = (
                line.x + glyph.x as f64 * layout.scale,
                line.baseline - glyph.y as f64 * layout.scale,
            );
            let settings = DrawSettings::unhinted(Size::unscaled(), LocationRef::default());
            outline.draw(settings, &mut pen).map_err(|e| {
                TextError::Font(format!("glyph {} could not be drawn: {e}", glyph.id))
            })?;
            pen.close();
        }
    }
    let [r, g, b, a] = [text.color.r, text.color.g, text.color.b, text.color.a];
    let color = premultiply([r, g, b, a].map(|c| f32::from(c) / 255.0));
    let (image, left, top) =
        fill(&pen.lines, color, limits.max_pixels).map_err(TextError::TooLarge)?;
    Ok(Drawn {
        image,
        box_width: layout.box_width,
        box_height: layout.box_height,
        origin_x: -left,
        origin_y: -top,
    })
}

/// Characters that end a line: UAX #14 mandatory breaks.
fn is_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r' | '\u{0B}' | '\u{0C}' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

/// Splits text at mandatory breaks, dropping the break characters. Text that
/// ends in a break has an empty last paragraph.
pub fn paragraphs(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (pos, opportunity) in linebreaks(text) {
        if opportunity == BreakOpportunity::Mandatory {
            out.push(text[start..pos].trim_end_matches(is_break));
            start = pos;
        }
    }
    if out.is_empty() || text.ends_with(is_break) {
        out.push("");
    }
    out
}

/// Greedy wrapping: each line ends at the last break opportunity where it
/// still fits in `max_units`, trailing white space not counted. Widths come
/// from shaping the whole paragraph once. A piece wider than the box gets a
/// line of its own and is not split.
fn wrap(
    paragraph: &str,
    shaper: &Shaper,
    bidi: &ParagraphBidiInfo,
    max_units: f64,
) -> Vec<Range<usize>> {
    let (glyphs, _) = shape(shaper, paragraph, bidi, 0..paragraph.len());
    let mut before = vec![0i64; paragraph.len() + 1];
    for glyph in glyphs {
        before[glyph.cluster as usize + 1] += glyph.advance;
    }
    for i in 1..before.len() {
        before[i] += before[i - 1];
    }
    let fits = |start: usize, end: usize| {
        let end = start + paragraph[start..end].trim_end().len();
        (before[end] - before[start]) as f64 <= max_units
    };

    let mut lines = Vec::new();
    let mut start = 0;
    let mut fit = None;
    for (pos, _) in linebreaks(paragraph) {
        if fits(start, pos) {
            fit = Some(pos);
            continue;
        }
        if let Some(end) = fit.take() {
            lines.push(start..end);
            start = end;
            if fits(start, pos) {
                fit = Some(pos);
                continue;
            }
        }
        lines.push(start..pos);
        start = pos;
    }
    if start < paragraph.len() || lines.is_empty() {
        lines.push(start..paragraph.len());
    }
    lines
}

/// Shapes `range` of a paragraph as one line: bidi runs in visual order, each
/// split into script runs and shaped in its own direction. Returns the glyphs
/// and the total advance.
fn shape(
    shaper: &Shaper,
    paragraph: &str,
    bidi: &ParagraphBidiInfo,
    range: Range<usize>,
) -> (Vec<Glyph>, i64) {
    let mut glyphs = Vec::new();
    let mut pen = 0i64;
    if range.is_empty() {
        return (glyphs, pen);
    }
    let (levels, runs) = bidi.visual_runs(range);
    for run in runs {
        let rtl = levels[run.start].is_rtl();
        let mut pieces = script_runs(paragraph, run);
        if rtl {
            pieces.reverse();
        }
        for (piece, script) in pieces {
            let mut buffer = UnicodeBuffer::new();
            for (i, c) in paragraph[piece.clone()].char_indices() {
                buffer.add(c, (piece.start + i) as u32);
            }
            buffer.set_direction(if rtl {
                Direction::RightToLeft
            } else {
                Direction::LeftToRight
            });
            if let Some(script) = script {
                buffer.set_script(script);
            }
            buffer.guess_segment_properties();
            let shaped = shaper.shape(buffer, ShapeOptions::new());
            for (info, pos) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
                glyphs.push(Glyph {
                    id: info.glyph_id,
                    cluster: info.cluster,
                    x: pen + i64::from(pos.x_offset),
                    y: i64::from(pos.y_offset),
                    advance: i64::from(pos.x_advance),
                });
                pen += i64::from(pos.x_advance);
            }
        }
    }
    (glyphs, pen)
}

/// Splits `range` into runs of one script, in logical order. Common and
/// inherited characters join the run before them, or the first run.
fn script_runs(text: &str, range: Range<usize>) -> Vec<(Range<usize>, Option<harfrust::Script>)> {
    let mut runs: Vec<(Range<usize>, Option<Script>)> = Vec::new();
    for (i, c) in text[range.clone()].char_indices() {
        let script = c.script();
        let real = !matches!(script, Script::Common | Script::Inherited | Script::Unknown);
        let (start, end) = (range.start + i, range.start + i + c.len_utf8());
        match runs.last_mut() {
            Some((run, current)) if !real || current.is_none() || *current == Some(script) => {
                run.end = end;
                if real {
                    *current = Some(script);
                }
            }
            _ => runs.push((start..end, real.then_some(script))),
        }
    }
    runs.into_iter()
        .map(|(run, script)| {
            let script = script.and_then(|s| {
                let name: [u8; 4] = s.short_name().as_bytes().try_into().ok()?;
                harfrust::Script::from_iso15924_tag(harfrust::Tag::new(&name))
            });
            (run, script)
        })
        .collect()
}

/// Collects glyph outlines as straight segments in layer box pixels.
#[derive(Default)]
struct Segments {
    scale: f64,
    origin: (f64, f64),
    start: (f64, f64),
    last: (f64, f64),
    open: bool,
    lines: Vec<[f64; 4]>,
}

/// Curves are cut into straight pieces no further than this from the curve, in pixels.
const FLATNESS: f64 = 0.05;
const MAX_PIECES: u32 = 256;

impl Segments {
    fn point(&self, x: f32, y: f32) -> (f64, f64) {
        (
            self.origin.0 + f64::from(x) * self.scale,
            self.origin.1 - f64::from(y) * self.scale,
        )
    }

    fn segment(&mut self, to: (f64, f64)) {
        if to != self.last {
            self.lines.push([self.last.0, self.last.1, to.0, to.1]);
        }
        self.last = to;
    }

    /// Straight pieces for a curve whose second differences are at most `bend`.
    fn pieces(bend: f64) -> u32 {
        ((bend / FLATNESS).sqrt().ceil() as u32).clamp(1, MAX_PIECES)
    }
}

impl OutlinePen for Segments {
    fn move_to(&mut self, x: f32, y: f32) {
        self.close();
        self.start = self.point(x, y);
        self.last = self.start;
        self.open = true;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let to = self.point(x, y);
        self.segment(to);
    }

    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let (p0, p1, p2) = (self.last, self.point(cx, cy), self.point(x, y));
        // A piece of parameter length h strays at most |p0 - 2p1 + p2| h² / 4.
        let bend = (p0.0 - 2.0 * p1.0 + p2.0).hypot(p0.1 - 2.0 * p1.1 + p2.1) / 4.0;
        let n = Self::pieces(bend);
        for i in 1..=n {
            let t = f64::from(i) / f64::from(n);
            let u = 1.0 - t;
            let q = |a: f64, b: f64, c: f64| u * u * a + 2.0 * u * t * b + t * t * c;
            self.segment((q(p0.0, p1.0, p2.0), q(p0.1, p1.1, p2.1)));
        }
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (p0, p1, p2, p3) = (
            self.last,
            self.point(cx0, cy0),
            self.point(cx1, cy1),
            self.point(x, y),
        );
        // A piece strays at most 3/4 · max(|p0 - 2p1 + p2|, |p1 - 2p2 + p3|) h².
        let d1 = (p0.0 - 2.0 * p1.0 + p2.0).hypot(p0.1 - 2.0 * p1.1 + p2.1);
        let d2 = (p1.0 - 2.0 * p2.0 + p3.0).hypot(p1.1 - 2.0 * p2.1 + p3.1);
        let n = Self::pieces(0.75 * d1.max(d2));
        for i in 1..=n {
            let t = f64::from(i) / f64::from(n);
            let u = 1.0 - t;
            let c = |a: f64, b: f64, c: f64, d: f64| {
                u * u * u * a + 3.0 * u * u * t * b + 3.0 * u * t * t * c + t * t * t * d
            };
            self.segment((c(p0.0, p1.0, p2.0, p3.0), c(p0.1, p1.1, p2.1, p3.1)));
        }
    }

    fn close(&mut self) {
        if self.open {
            self.segment(self.start);
            self.open = false;
        }
    }
}

/// Fills closed outlines made of `lines` (`[x0, y0, x1, y1]`) with `color`
/// (premultiplied), anti-aliased by area coverage with overlapping windings
/// added and capped at full. Returns the image and the pixel position of its
/// top-left corner.
pub fn fill(
    lines: &[[f64; 4]],
    color: [f32; 4],
    max_pixels: u64,
) -> Result<(Pixmap, i64, i64), TooLarge> {
    let finite = || lines.iter().flatten().all(|v| v.is_finite());
    if lines.is_empty() || !finite() {
        return Ok((Pixmap::filled(0, 0, [0.0; 4], max_pixels)?, 0, 0));
    }
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for [x0, y0, x1, y1] in lines {
        min_x = min_x.min(*x0).min(*x1);
        max_x = max_x.max(*x0).max(*x1);
        min_y = min_y.min(*y0).min(*y1);
        max_y = max_y.max(*y0).max(*y1);
    }
    let (left, top) = (min_x.floor(), min_y.floor());
    let size = |extent: f64| u32::try_from(extent as u64).unwrap_or(u32::MAX);
    let (w, h) = (size(max_x.ceil() - left), size(max_y.ceil() - top));
    let mut image = Pixmap::filled(w, h, [0.0; 4], max_pixels)?;

    let stride = w as usize + 2;
    let mut area = vec![0.0f64; stride * h as usize];
    for [x0, y0, x1, y1] in lines {
        accumulate(
            &mut area,
            stride,
            h as usize,
            [x0 - left, y0 - top, x1 - left, y1 - top],
        );
    }
    for (y, row) in area.chunks_exact(stride).enumerate() {
        let mut sum = 0.0;
        for (x, a) in row[..w as usize].iter().enumerate() {
            sum += a;
            let coverage = sum.abs().min(1.0) as f32;
            if coverage > 0.0 {
                image.set(x as u32, y as u32, color.map(|c| c * coverage));
            }
        }
    }
    Ok((image, left as i64, top as i64))
}

/// Adds one edge's signed area to the rows it crosses. Summing a row left to
/// right then gives each pixel's coverage.
fn accumulate(area: &mut [f64], stride: usize, height: usize, [x0, y0, x1, y1]: [f64; 4]) {
    if y0 == y1 {
        return;
    }
    let (dir, x0, y0, x1, y1) = if y0 < y1 {
        (1.0, x0, y0, x1, y1)
    } else {
        (-1.0, x1, y1, x0, y0)
    };
    let dxdy = (x1 - x0) / (y1 - y0);
    let last_row = (y1.ceil() as usize).min(height);
    for row in (y0.floor() as usize)..last_row {
        let top = (row as f64).max(y0);
        let bottom = ((row + 1) as f64).min(y1);
        if bottom <= top {
            continue;
        }
        let xa = x0 + (top - y0) * dxdy;
        let xb = x0 + (bottom - y0) * dxdy;
        let d = dir * (bottom - top);
        let (l, r) = if xa < xb { (xa, xb) } else { (xb, xa) };
        let line = &mut area[row * stride..(row + 1) * stride];
        let l_floor = l.floor();
        let li = l_floor as usize;
        let r_ceil = r.ceil();
        let ri = r_ceil as usize;
        if ri <= li + 1 {
            // The edge stays inside one pixel column.
            let mid = 0.5 * (xa + xb) - l_floor;
            line[li] += d * (1.0 - mid);
            line[li + 1] += d * mid;
        } else {
            let s = 1.0 / (r - l);
            let l_frac = l - l_floor;
            let a0 = 0.5 * s * (1.0 - l_frac) * (1.0 - l_frac);
            let r_frac = r - r_ceil + 1.0;
            let am = 0.5 * s * r_frac * r_frac;
            line[li] += d * a0;
            if ri == li + 2 {
                line[li + 1] += d * (1.0 - a0 - am);
            } else {
                let a1 = s * (1.5 - l_frac);
                line[li + 1] += d * (a1 - a0);
                for cell in &mut line[li + 2..ri - 1] {
                    *cell += d * s;
                }
                let a2 = a1 + (ri - li - 3) as f64 * s;
                line[ri - 1] += d * (1.0 - a2 - am);
            }
            line[ri] += d * am;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unbaked_core::recipe::Color;

    const LATO: &[u8] = include_bytes!("../../../tests/fonts/Lato-Regular.ttf");

    fn pixels(max_pixels: u64) -> RenderLimits {
        RenderLimits {
            max_pixels,
            ..RenderLimits::default()
        }
    }

    fn text(s: &str) -> Text {
        Text {
            text: s.into(),
            font: "lato".into(),
            size_px: 20.0,
            color: Color {
                r: 0,
                g: 0,
                b: 0,
                a: 255,
            },
            line_height: 1.2,
            align: Align::Left,
            box_width: None,
            font_index: 0,
        }
    }

    fn lay(t: &Text) -> Layout {
        layout(t, &font(LATO, 0).unwrap()).unwrap()
    }

    fn polygon(points: &[(f64, f64)]) -> Vec<[f64; 4]> {
        (0..points.len())
            .map(|i| {
                let (a, b) = (points[i], points[(i + 1) % points.len()]);
                [a.0, a.1, b.0, b.1]
            })
            .collect()
    }

    fn coverage(lines: &[[f64; 4]]) -> (Pixmap, i64, i64) {
        fill(lines, [1.0; 4], 10_000).unwrap()
    }

    #[test]
    fn fill_covers_exact_areas() {
        let (image, left, top) =
            coverage(&polygon(&[(0.5, 0.5), (2.5, 0.5), (2.5, 2.5), (0.5, 2.5)]));
        assert_eq!((image.width, image.height, left, top), (3, 3, 0, 0));
        assert_eq!(image.pixel(0, 0)[3], 0.25);
        assert_eq!(image.pixel(1, 0)[3], 0.5);
        assert_eq!(image.pixel(1, 1)[3], 1.0);

        // The triangle under y = 4 - x: its edge halves the pixels it crosses diagonally.
        let (image, ..) = coverage(&polygon(&[(0.0, 0.0), (4.0, 0.0), (0.0, 4.0)]));
        let total: f32 = image.data.as_chunks::<4>().0.iter().map(|p| p[3]).sum();
        assert!((total - 8.0).abs() < 1e-5, "{total}");
        assert_eq!(image.pixel(0, 0)[3], 1.0);
        assert!((image.pixel(3, 0)[3] - 0.5).abs() < 1e-6);
        assert!((image.pixel(1, 2)[3] - 0.5).abs() < 1e-6);
        assert_eq!(image.pixel(2, 2)[3], 0.0);

        // A shallow edge spanning many columns in one row.
        let (image, ..) = coverage(&polygon(&[(0.0, 0.0), (8.0, 0.0), (8.0, 1.0)]));
        for x in 0..8 {
            let expected = (x as f32 + 0.5) / 8.0;
            assert!((image.pixel(x, 0)[3] - expected).abs() < 1e-6, "{x}");
        }
    }

    #[test]
    fn fill_uses_windings() {
        // Same direction overlapping: capped at full. Opposite direction inside: a hole.
        let outer = polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)]);
        let mut both = outer.clone();
        both.extend(polygon(&[(1.0, 1.0), (3.0, 1.0), (3.0, 3.0), (1.0, 3.0)]));
        assert_eq!(coverage(&both).0.pixel(1, 1)[3], 1.0);
        let mut hole = outer;
        hole.extend(polygon(&[(1.0, 1.0), (1.0, 3.0), (3.0, 3.0), (3.0, 1.0)]));
        let (image, ..) = coverage(&hole);
        assert_eq!(image.pixel(1, 1)[3], 0.0);
        assert_eq!(image.pixel(0, 1)[3], 1.0);
    }

    #[test]
    fn fill_reports_position_and_limits() {
        let (image, left, top) = coverage(&polygon(&[(-3.5, 10.0), (-1.0, 10.0), (-1.0, 12.0)]));
        assert_eq!((left, top, image.width, image.height), (-4, 10, 3, 2));
        let huge = polygon(&[(0.0, 0.0), (1e6, 0.0), (0.0, 1e6)]);
        assert!(fill(&huge, [1.0; 4], 10_000).is_err());
        assert_eq!(coverage(&[]).0.width, 0);
    }

    #[test]
    fn paragraphs_split_at_mandatory_breaks() {
        assert_eq!(paragraphs("a\nb"), ["a", "b"]);
        assert_eq!(paragraphs("a\r\nb\u{2028}c"), ["a", "b", "c"]);
        assert_eq!(paragraphs("a\n"), ["a", ""]);
        assert_eq!(paragraphs("a\n\nb"), ["a", "", "b"]);
        assert_eq!(paragraphs(""), [""]);
        assert_eq!(paragraphs("one line"), ["one line"]);
    }

    #[test]
    fn box_follows_hhea_metrics_and_line_height() {
        let f = font(LATO, 0).unwrap();
        let hhea = f.hhea().unwrap();
        let (asc, desc) = (hhea.ascender().to_i16(), hhea.descender().to_i16());
        let upem = f.head().unwrap().units_per_em();
        let scale = 20.0 / f64::from(upem);

        let out = lay(&text("Hi\nthere"));
        assert_eq!(out.lines.len(), 2);
        assert!((out.box_height - 48.0).abs() < 1e-9);
        let first = (24.0 - (f64::from(asc) + f64::from(desc).abs()) * scale) / 2.0
            + f64::from(asc) * scale;
        assert!((out.lines[0].baseline - first).abs() < 1e-9);
        assert!((out.lines[1].baseline - first - 24.0).abs() < 1e-9);
        let widest = out.lines.iter().map(|l| l.width).max().unwrap();
        assert!((out.box_width - widest as f64 * scale).abs() < 1e-9);
        assert_eq!(out.lines[1].width, widest, "\"there\" is the wider line");
    }

    #[test]
    fn shaping_applies_kerning() {
        let width = |s: &str| lay(&text(s)).lines[0].width;
        assert!(
            width("AV") < width("A") + width("V"),
            "{} vs {}",
            width("AV"),
            width("A") + width("V")
        );
    }

    #[test]
    fn trailing_space_is_not_measured() {
        let a = lay(&text("word"));
        let b = lay(&text("word   "));
        assert_eq!(a.lines[0].width, b.lines[0].width);
        assert!(lay(&text("  word")).lines[0].width > a.lines[0].width);
    }

    #[test]
    fn wrapping_is_greedy_and_never_splits_words() {
        let one = |s: &str| lay(&text(s)).lines[0].width as f64 * lay(&text(s)).scale;
        let mut t = text("aaa bbb ccc");
        // Room for "aaa bbb" but not "aaa bbb ccc".
        t.box_width = Some(one("aaa bbb") + 0.01);
        let out = lay(&t);
        assert_eq!(out.lines.len(), 2);
        assert_eq!(out.lines[0].glyphs.len(), 7);
        assert_eq!(out.lines[1].glyphs.len(), 3);
        assert_eq!(out.lines[1].glyphs[0].cluster, 8);
        assert_eq!(out.box_width, t.box_width.unwrap());

        // A box narrower than every word: one word per line, none split.
        t.box_width = Some(1.0);
        let out = lay(&t);
        assert_eq!(out.lines.len(), 3);
        assert!(out.lines.iter().all(|l| l.glyphs.len() == 3));
    }

    #[test]
    fn align_places_each_line_in_the_box() {
        let mut t = text("wide line\nx");
        t.align = Align::Right;
        let out = lay(&t);
        assert_eq!(out.lines[0].x, 0.0);
        let right = out.lines[1].x + out.lines[1].width as f64 * out.scale;
        assert!((right - out.box_width).abs() < 1e-9);
        t.align = Align::Center;
        let out = lay(&t);
        let spare = out.box_width - out.lines[1].width as f64 * out.scale;
        assert!((out.lines[1].x - spare / 2.0).abs() < 1e-9);
    }

    #[test]
    fn right_to_left_runs_are_reordered() {
        // "ab אבג cd": the Hebrew run reads right to left inside a left-to-right line.
        let s = "ab \u{5D0}\u{5D1}\u{5D2} cd";
        let clusters: Vec<u32> = lay(&text(s)).lines[0]
            .glyphs
            .iter()
            .map(|g| g.cluster)
            .collect();
        let alef = 3;
        let (bet, gimel) = (alef + 2, alef + 4);
        let pos = |c: u32| clusters.iter().position(|&x| x == c).unwrap();
        assert!(pos(0) < pos(gimel) && pos(gimel) < pos(bet) && pos(bet) < pos(alef));
        assert!(pos(alef) < pos(s.find('c').unwrap() as u32));

        // A paragraph starting with Hebrew is right to left: its first letter is drawn last.
        let s = "\u{5D0}\u{5D1} ab";
        let glyphs = &lay(&text(s)).lines[0].glyphs;
        assert_eq!(glyphs.last().unwrap().cluster, 0);
    }

    #[test]
    fn draw_fills_glyphs_in_colour() {
        let mut t = text("Hi");
        t.color = Color {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let drawn = draw(&t, LATO, pixels(1_000_000)).unwrap();
        let solid = drawn
            .image
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] == 1.0)
            .count();
        assert!(solid > 20, "{solid} fully covered pixels");
        assert!(
            drawn
                .image
                .data
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| p[1] == 0.0 && p[0] == p[3]),
            "only premultiplied red"
        );
        // The ink sits inside the box here, so the image starts at or after its corner.
        assert!(drawn.origin_x <= 0 && drawn.origin_y <= 0);
        assert!(drawn.image.width as f64 <= drawn.box_width + 2.0);

        let empty = draw(&text(""), LATO, pixels(1_000_000)).unwrap();
        assert_eq!(empty.image.width, 0);
        assert!((empty.box_height - 24.0).abs() < 1e-9);
    }

    #[test]
    fn bad_fonts_are_errors() {
        assert!(matches!(font(b"not a font", 0), Err(TextError::Font(_))));
        assert!(matches!(font(LATO, 3), Err(TextError::Font(_))));
    }
}
