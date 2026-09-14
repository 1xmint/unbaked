//! Quick looks for an agent or an editor: a still picture of any moment, a
//! contact sheet of a video, and numbers that describe a sound mix. None of
//! them writes an Unbaked file.

use unbaked_core::pack;
use unbaked_core::recipe::{Blend, OutputKind};

use crate::draw::{Affine, place};
use crate::image::{Pixmap, encode_png};
use crate::scene::{Frames, RenderLimits};
use crate::sound::{self, Pcm};
use crate::timing::{Moment, frame_count};
use crate::{FontSource, RenderError, checked_recipe};

/// What to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewOptions {
    /// The moment to show. Defaults to `output.at_ms` for an image and 0 for a video.
    pub at_ms: Option<u64>,
    /// The longest edge of the result, in pixels. The picture is only ever shrunk.
    pub max_edge: u32,
    /// For a video: this many evenly spaced frames in one grid, first frame first.
    pub sheet: Option<u32>,
}

impl Default for PreviewOptions {
    /// 1024 pixels, a size vision models read well.
    fn default() -> Self {
        PreviewOptions {
            at_ms: None,
            max_edge: 1024,
            sheet: None,
        }
    }
}

/// Most frames in one contact sheet.
pub const MAX_SHEET: u32 = 64;

/// Pixels between frames in a contact sheet.
const GAP: u32 = 4;

/// A plain PNG of an image or video recipe. Images keep their transparency;
/// video frames are shown over black, as they are encoded.
pub fn preview(
    files: &pack::Files,
    fonts: &dyn FontSource,
    limits: RenderLimits,
    options: PreviewOptions,
) -> Result<Pixmap, RenderError> {
    let recipe = checked_recipe(files)?;
    let file = |path: &str| files.get(path).map(Vec::as_slice);
    let output = &recipe.output;
    let video = match output.kind {
        OutputKind::Image => false,
        OutputKind::Video => true,
        OutputKind::Audio => {
            return Err(RenderError::Unsupported(
                "a sound recipe has no picture; use listen".into(),
            ));
        }
    };
    let mut frames = Frames::new(&recipe, &file, fonts, limits)?;
    let mut draw = |moment: Moment| -> Result<Pixmap, RenderError> {
        let mut canvas = frames.draw(moment)?;
        if video {
            // Premultiplied colour over opaque black is the colour itself.
            for px in canvas.data.as_chunks_mut::<4>().0 {
                px[3] = 1.0;
            }
        }
        Ok(canvas)
    };

    let Some(count) = options.sheet else {
        let at = options
            .at_ms
            .unwrap_or(if video { 0 } else { output.at_ms });
        let canvas = draw(Moment::AtMs(at))?;
        return shrink(&canvas, options.max_edge, limits);
    };
    let (Some(fps), Some(duration_ms), true) = (output.fps, output.duration_ms, video) else {
        return Err(RenderError::Unsupported(
            "a contact sheet needs a video recipe".into(),
        ));
    };
    if count == 0 || count > MAX_SHEET {
        return Err(RenderError::Unsupported(format!(
            "a contact sheet holds 1 to {MAX_SHEET} frames"
        )));
    }
    let total = frame_count(duration_ms, fps).max(1);
    let columns = (f64::from(count).sqrt().ceil() as u32).max(1);
    let rows = count.div_ceil(columns);
    let (width, height) = (
        output.width.unwrap_or(1).max(1),
        output.height.unwrap_or(1).max(1),
    );
    // Shrink each frame so the whole grid fits `max_edge`.
    let across = u64::from(columns) * u64::from(width) + u64::from(GAP) * u64::from(columns - 1);
    let down = u64::from(rows) * u64::from(height) + u64::from(GAP) * u64::from(rows - 1);
    let scale = (f64::from(options.max_edge) / across.max(down) as f64).min(1.0);
    let (cell_w, cell_h) = (
        ((f64::from(width) * scale).round() as u32).max(1),
        ((f64::from(height) * scale).round() as u32).max(1),
    );
    let mut sheet = Pixmap::filled(
        columns * cell_w + GAP * (columns - 1),
        rows * cell_h + GAP * (rows - 1),
        [0.2, 0.2, 0.2, 1.0],
        limits.max_pixels,
    )
    .map_err(|e| RenderError::TooLarge {
        what: "the contact sheet".into(),
        pixels: e.pixels,
        limit: e.limit,
    })?;
    for i in 0..count {
        let n = u64::from(i) * total / u64::from(count);
        let canvas = draw(Moment::Frame { n, fps })?;
        let (x, y) = (i % columns * (cell_w + GAP), i / columns * (cell_h + GAP));
        let to_sheet = Affine::translate(f64::from(x), f64::from(y)).then_after(Affine::scale(
            f64::from(cell_w) / f64::from(width),
            f64::from(cell_h) / f64::from(height),
        ));
        place(&mut sheet, &canvas, to_sheet, 1.0, Blend::Normal);
    }
    Ok(sheet)
}

/// [`preview`] encoded as a PNG.
pub fn preview_png(
    files: &pack::Files,
    fonts: &dyn FontSource,
    limits: RenderLimits,
    options: PreviewOptions,
) -> Result<Vec<u8>, RenderError> {
    let pixels = preview(files, fonts, limits, options)?;
    encode_png(pixels.width, pixels.height, &pixels.to_rgba8()).map_err(RenderError::Encode)
}

/// The canvas scaled down so its longest edge is at most `max_edge`.
fn shrink(canvas: &Pixmap, max_edge: u32, limits: RenderLimits) -> Result<Pixmap, RenderError> {
    let longest = canvas.width.max(canvas.height);
    let max_edge = max_edge.max(1);
    if longest <= max_edge {
        return Ok(canvas.clone());
    }
    let scale = f64::from(max_edge) / f64::from(longest);
    let (w, h) = (
        ((f64::from(canvas.width) * scale).round() as u32).max(1),
        ((f64::from(canvas.height) * scale).round() as u32).max(1),
    );
    let mut out =
        Pixmap::filled(w, h, [0.0; 4], limits.max_pixels).map_err(|e| RenderError::TooLarge {
            what: "the preview".into(),
            pixels: e.pixels,
            limit: e.limit,
        })?;
    let to_out = Affine::scale(
        f64::from(w) / f64::from(canvas.width),
        f64::from(h) / f64::from(canvas.height),
    );
    place(&mut out, canvas, to_out, 1.0, Blend::Normal);
    Ok(out)
}

/// What a sound mix sounds like, in numbers.
#[derive(Debug, Clone, PartialEq)]
pub struct Stats {
    pub duration_ms: u64,
    pub sample_rate: u32,
    pub channels: usize,
    /// The loudest sample, in dB below full scale. Minus infinity for silence.
    pub peak_dbfs: f64,
    /// Samples at full scale: the mix is clipped to -1..1, so these were cut off.
    pub clipped_samples: u64,
    /// RMS level of each [`WINDOW_MS`] window, in dBFS, across all channels.
    pub loudness_dbfs: Vec<f64>,
    /// Stretches of at least [`SILENT_MS`] where every sample is below [`SILENCE`].
    pub silences: Vec<Silence>,
}

/// A quiet stretch, in milliseconds from the start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Silence {
    pub start_ms: u64,
    pub end_ms: u64,
}

/// The length of one loudness window.
pub const WINDOW_MS: u64 = 500;
/// The shortest stretch reported as silent.
pub const SILENT_MS: u64 = 250;
/// Samples below this magnitude (-60 dBFS) count as silent.
pub const SILENCE: f32 = 0.001;

/// Mixes the sound of an audio or video recipe and describes it.
pub fn listen(files: &pack::Files, limits: RenderLimits) -> Result<Stats, RenderError> {
    let recipe = checked_recipe(files)?;
    if recipe.output.kind == OutputKind::Image {
        return Err(RenderError::Unsupported(
            "an image recipe has no sound; use preview".into(),
        ));
    }
    let file = |path: &str| files.get(path).map(Vec::as_slice);
    let pcm = sound::mix(&recipe, &file, limits)?;
    Ok(stats(&pcm))
}

fn dbfs(level: f64) -> f64 {
    if level > 0.0 {
        20.0 * level.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// Describes decoded sound.
pub fn stats(pcm: &Pcm) -> Stats {
    let rate = u64::from(pcm.rate.max(1));
    let len = pcm.len();
    let mut peak = 0.0f32;
    let mut clipped = 0;
    for &s in pcm.channels.iter().flatten() {
        peak = peak.max(s.abs());
        if s.abs() >= 1.0 {
            clipped += 1;
        }
    }
    let window = ((WINDOW_MS * rate) / 1000).max(1) as usize;
    let loudness = (0..len)
        .step_by(window)
        .map(|start| {
            let end = (start + window).min(len);
            let (sum, n) = pcm.channels.iter().fold((0.0f64, 0usize), |(sum, n), c| {
                let part = &c[start..end];
                let squares: f64 = part.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
                (sum + squares, n + part.len())
            });
            dbfs((sum / n.max(1) as f64).sqrt())
        })
        .collect();

    let quiet = |k: usize| pcm.channels.iter().all(|c| c[k].abs() < SILENCE);
    let to_ms = |k: usize| k as u64 * 1000 / rate;
    let mut silences = Vec::new();
    let mut start = None;
    for k in 0..=len {
        match (k < len && quiet(k), start) {
            (true, None) => start = Some(k),
            (false, Some(s)) => {
                if (k - s) as u64 * 1000 >= SILENT_MS * rate {
                    silences.push(Silence {
                        start_ms: to_ms(s),
                        end_ms: to_ms(k),
                    });
                }
                start = None;
            }
            _ => {}
        }
    }
    Stats {
        duration_ms: len as u64 * 1000 / rate,
        sample_rate: pcm.rate,
        channels: pcm.channels.len(),
        peak_dbfs: dbfs(f64::from(peak)),
        clipped_samples: clipped,
        loudness_dbfs: loudness,
        silences,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_describe_level_clipping_and_silence() {
        // One second at 1000 Hz: 300 ms silent, then half level, then 10 clipped samples.
        let mut left = vec![0.0f32; 1000];
        for s in &mut left[300..990] {
            *s = 0.5;
        }
        for s in &mut left[990..] {
            *s = 1.0;
        }
        let pcm = Pcm {
            rate: 1000,
            channels: vec![left.clone(), left],
        };
        let stats = stats(&pcm);
        assert_eq!((stats.duration_ms, stats.channels), (1000, 2));
        assert_eq!(stats.peak_dbfs, 0.0);
        assert_eq!(stats.clipped_samples, 20);
        assert_eq!(stats.loudness_dbfs.len(), 2);
        assert!((stats.loudness_dbfs[0] - dbfs((0.25f64 * 200.0 / 500.0).sqrt())).abs() < 1e-9);
        assert_eq!(
            stats.silences,
            [Silence {
                start_ms: 0,
                end_ms: 300
            }]
        );
        let silent = stats_of_silence();
        assert_eq!(silent.peak_dbfs, f64::NEG_INFINITY);
    }

    fn stats_of_silence() -> Stats {
        stats(&Pcm {
            rate: 48_000,
            channels: vec![vec![0.0; 100]],
        })
    }
}
