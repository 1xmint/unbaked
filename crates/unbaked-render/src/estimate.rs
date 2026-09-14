//! The size of a render, worked out before it runs, from the recipe and asset
//! headers alone. A service that states a price up front uses this; nothing
//! here decodes pixels or sound.

use std::collections::BTreeSet;
use std::io::Cursor;

use unbaked_core::pack;
use unbaked_core::recipe::{AssetSource, Content, Effect, Layer, OutputKind};
use unbaked_core::rules;
use unbaked_core::sniff::{self, AssetKind};
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

use crate::scene::RenderLimits;
use crate::sound;
use crate::timing::frame_count;
use crate::{RenderError, checked_recipe, mp4};

/// How big a render is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Estimate {
    pub kind: OutputKind,
    /// Width times height, 0 for sound.
    pub canvas_pixels: u64,
    /// 1 for an image, every frame for a video, 0 for sound.
    pub frames: u64,
    /// Visible layers, counting those inside groups and masks.
    pub layers: u64,
    /// Buffers drawn per frame besides the canvas: groups, masks, and the two
    /// passes of each blur or shadow.
    pub extra_buffers: u64,
    /// Blur and shadow effects.
    pub blurs: u64,
    /// The largest blur radius in pixels, `ceil(3σ)` at the largest σ.
    pub max_blur_radius: u64,
    /// Pixels of every image asset a layer uses, decoded.
    pub image_pixels: u64,
    /// Pixels of video frames decoded per output frame, over all video layers.
    pub video_pixels_per_frame: u64,
    /// Samples in the mix, all channels.
    pub output_samples: u64,
    /// Samples of every sound source a clip uses, all channels, before resampling.
    pub source_samples: u64,
    /// Bytes of every file the recipe references.
    pub asset_bytes: u64,
    /// One number for the whole job: about a million pixel or sample steps
    /// each. Only comparable between estimates from the same renderer version.
    pub work_units: u64,
}

/// Estimates a package's render.
pub fn estimate(files: &pack::Files) -> Result<Estimate, RenderError> {
    let recipe = checked_recipe(files)?;
    let output = &recipe.output;
    let asset_file = |id: &str| -> Result<(&str, &[u8]), RenderError> {
        match recipe.assets.get(id).map(|a| &a.source) {
            Some(AssetSource::Path(path)) => files
                .get(path)
                .map(|data| (path.as_str(), data.as_slice()))
                .ok_or_else(|| RenderError::Unsupported(format!("{path} is missing"))),
            _ => Err(RenderError::Unsupported(format!(
                "asset {id:?} is not a file"
            ))),
        }
    };

    let mut walk = Walk::default();
    let (canvas_pixels, frames) = match output.kind {
        OutputKind::Audio => (0, 0),
        kind => {
            walk.layers(&recipe.layers);
            let pixels =
                u64::from(output.width.unwrap_or(0)) * u64::from(output.height.unwrap_or(0));
            let frames = match (kind, output.fps, output.duration_ms) {
                (OutputKind::Video, Some(fps), Some(duration)) => frame_count(duration, fps),
                _ => 1,
            };
            (pixels, frames)
        }
    };

    let mut image_pixels = 0u64;
    for id in &walk.images {
        let (path, data) = asset_file(id)?;
        let (w, h) = image_size(data).map_err(|message| RenderError::Decode {
            asset: path.to_owned(),
            message,
        })?;
        image_pixels = image_pixels.saturating_add(u64::from(w) * u64::from(h));
    }
    let mut video_pixels_per_frame = 0u64;
    for id in &walk.videos {
        let (path, data) = asset_file(id)?;
        let (w, h) = video_size(data).map_err(|message| RenderError::Decode {
            asset: path.to_owned(),
            message,
        })?;
        video_pixels_per_frame += u64::from(w) * u64::from(h);
    }

    let has_sound = output.kind == OutputKind::Audio || !recipe.audio.is_empty();
    let channels = u64::from(output.channels);
    let output_samples = if has_sound {
        (u128::from(output.duration_ms.unwrap_or(0)) * u128::from(output.sample_rate) / 1000)
            .try_into()
            .unwrap_or(u64::MAX)
            .saturating_mul(channels)
    } else {
        0
    };
    let sources: BTreeSet<&str> = recipe
        .audio
        .iter()
        .filter(|clip| !clip.muted)
        .map(|clip| clip.asset.as_str())
        .collect();
    let mut source_samples = 0u64;
    let mut resample_steps = 0u64;
    for id in sources {
        let (path, data) = asset_file(id)?;
        let length =
            sound::length(data, RenderLimits::default().max_samples).map_err(|message| {
                RenderError::Decode {
                    asset: path.to_owned(),
                    message,
                }
            })?;
        let samples = length.frames.saturating_mul(u64::from(length.channels));
        source_samples = source_samples.saturating_add(samples);
        // Resampling costs its kernel width times the longer of the two lengths.
        let stretch = u64::from(output.sample_rate).div_ceil(u64::from(length.rate.max(1)));
        resample_steps = resample_steps
            .saturating_add(samples.saturating_mul(sound::RESAMPLE_TAPS * stretch.max(1)));
    }
    let asset_bytes = unbaked_core::bake::referenced_files(&recipe)
        .into_iter()
        .filter_map(|path| files.get(path))
        .map(|data| data.len() as u64)
        .sum();

    let canvas_passes = 1 + walk.layers + walk.extra_buffers;
    let radius = walk.max_blur_radius;
    let side = |extent: Option<u32>| {
        u64::from(extent.unwrap_or(0)).saturating_add(radius.saturating_mul(2))
    };
    let blur_area = side(output.width).saturating_mul(side(output.height));
    let video_encode = if output.kind == OutputKind::Video {
        2
    } else {
        0
    };
    // Hostile recipes can ask for absurd sizes; the total saturates instead of overflowing.
    let product = |factors: &[u64]| factors.iter().fold(1u64, |a, &b| a.saturating_mul(b));
    let steps = [
        product(&[frames, canvas_pixels, canvas_passes + video_encode]),
        product(&[
            frames,
            walk.blurs,
            blur_area,
            2,
            radius.saturating_mul(2).saturating_add(1),
        ]),
        product(&[frames, video_pixels_per_frame]),
        image_pixels,
        resample_steps,
        product(&[output_samples, 1 + recipe.audio.len() as u64]),
    ]
    .into_iter()
    .fold(0u64, u64::saturating_add);

    Ok(Estimate {
        kind: output.kind,
        canvas_pixels,
        frames,
        layers: walk.layers,
        extra_buffers: walk.extra_buffers,
        blurs: walk.blurs,
        max_blur_radius: radius,
        image_pixels,
        video_pixels_per_frame,
        output_samples,
        source_samples,
        asset_bytes,
        work_units: steps.div_ceil(1_000_000).max(1),
    })
}

#[derive(Default)]
struct Walk<'a> {
    layers: u64,
    extra_buffers: u64,
    blurs: u64,
    max_blur_radius: u64,
    images: BTreeSet<&'a str>,
    videos: Vec<&'a str>,
}

impl<'a> Walk<'a> {
    fn layers(&mut self, layers: &'a [Layer]) {
        for layer in layers.iter().filter(|l| !l.hidden) {
            self.layers += 1;
            for effect in &layer.effects {
                if let Effect::Blur { sigma } | Effect::Shadow { sigma, .. } = effect {
                    let (_, most) = rules::range(sigma);
                    self.blurs += 1;
                    self.extra_buffers += 2;
                    self.max_blur_radius = self
                        .max_blur_radius
                        .max((3.0 * most.max(0.0)).ceil() as u64);
                }
            }
            if let Some(mask) = &layer.mask {
                self.extra_buffers += 2;
                self.layers(&mask.layers);
            }
            match &layer.content {
                Content::Group { layers } => {
                    self.extra_buffers += 1;
                    self.layers(layers);
                }
                Content::Image { asset, .. } => {
                    self.images.insert(asset);
                }
                Content::Video { asset, .. } => self.videos.push(asset),
                Content::Text(_) | Content::Solid { .. } => {}
            }
        }
    }
}

/// Width and height of an MP4's first video track, from its sample description.
fn video_size(data: &[u8]) -> Result<(u32, u32), String> {
    let movie = mp4::read(data, RenderLimits::default().max_samples)?;
    let track = movie
        .tracks
        .iter()
        .find(|t| &t.handler == b"vide")
        .ok_or("the file has no video track")?;
    // A visual sample entry: 24 bytes, then width and height as 16-bit numbers.
    let field = |at: usize| {
        track
            .entry
            .body
            .get(at..at + 2)
            .map(|b| u32::from(u16::from_be_bytes([b[0], b[1]])))
            .ok_or("bad video sample description")
    };
    Ok((field(24)?, field(26)?))
}

/// Image width and height from the file header.
fn image_size(data: &[u8]) -> Result<(u32, u32), String> {
    match sniff::detect(&data[..data.len().min(sniff::HEADER_LEN)]) {
        Some(AssetKind::Png) => {
            let reader = png::Decoder::new(Cursor::new(data))
                .read_info()
                .map_err(|e| e.to_string())?;
            Ok((reader.info().width, reader.info().height))
        }
        Some(AssetKind::Jpeg) => {
            let mut decoder =
                JpegDecoder::new_with_options(Cursor::new(data), DecoderOptions::default());
            decoder.decode_headers().map_err(|e| e.to_string())?;
            let info = decoder.info().ok_or("no JPEG header")?;
            Ok((u32::from(info.width), u32::from(info.height)))
        }
        Some(AssetKind::Webp) => {
            let decoder =
                image_webp::WebPDecoder::new(Cursor::new(data)).map_err(|e| e.to_string())?;
            Ok(decoder.dimensions())
        }
        _ => Err("not a PNG, JPEG or WebP image".into()),
    }
}
