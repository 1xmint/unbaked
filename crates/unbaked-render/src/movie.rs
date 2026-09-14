//! Video output (SPEC.md sections 5.2 and 5.7): every frame drawn, encoded as
//! H.264 and written to MP4, with the mixed sound as AAC-LC when the recipe
//! has audio clips.

use unbaked_core::recipe::{Fps, Recipe};

use crate::scene::{Frames, RenderLimits};
use crate::timing::{Moment, frame_count};
use crate::video::FrameEncoder;
use crate::{FontSource, RenderError, mp4, sound};

/// Renders a `video` recipe to an MP4 file. Frames are drawn and encoded one
/// at a time; only the encoded stream is kept.
pub fn render_video<'a>(
    recipe: &'a Recipe,
    file: &'a dyn Fn(&str) -> Option<&'a [u8]>,
    fonts: &'a dyn FontSource,
    limits: RenderLimits,
) -> Result<Vec<u8>, RenderError> {
    let output = &recipe.output;
    let (Some(width), Some(height), Some(fps), Some(duration_ms)) =
        (output.width, output.height, output.fps, output.duration_ms)
    else {
        return Err(RenderError::Unsupported(
            "a video needs width, height, fps and duration_ms".into(),
        ));
    };
    let frames = frame_count(duration_ms, fps);
    if frames > limits.max_frames {
        return Err(RenderError::TooManyFrames {
            frames,
            limit: limits.max_frames,
        });
    }
    let (timescale, delta) = track_timing(fps).ok_or_else(|| {
        RenderError::Unsupported(format!(
            "frame rate {}/{} does not fit an MP4 track",
            fps.num, fps.den
        ))
    })?;
    let (Ok(w), Ok(h)) = (u16::try_from(width), u16::try_from(height)) else {
        return Err(RenderError::Unsupported("the video is too large".into()));
    };

    let mut drawing = Frames::new(recipe, file, fonts, limits)?;
    let rate = fps.num as f64 / fps.den as f64;
    let mut encoder = FrameEncoder::new(width, height, rate).map_err(|message| {
        if message.contains("not rendered yet") {
            RenderError::Unsupported(message)
        } else {
            RenderError::Encode(message)
        }
    })?;
    for n in 0..frames {
        limits.check_time()?;
        let canvas = drawing.draw(Moment::Frame { n, fps })?;
        encoder.push(&canvas).map_err(RenderError::Encode)?;
    }
    drop(drawing);
    let stream = encoder.finish().map_err(RenderError::Encode)?;

    let sound = if recipe.audio.is_empty() {
        None
    } else {
        let pcm = sound::mix(recipe, file, limits)?;
        limits.check_time()?;
        let aac = sound::encode_aac(&pcm).map_err(RenderError::Encode)?;
        Some((pcm, aac))
    };
    let audio = sound.as_ref().map(|(pcm, aac)| sound::aac_track(pcm, aac));
    Ok(mp4::write_mp4(
        &mp4::VideoTrack {
            width: w,
            height: h,
            timescale,
            delta,
            sps: &stream.sps,
            pps: &stream.pps,
            samples: &stream.samples,
            sync: &stream.sync,
            length_ms: duration_ms,
        },
        audio.as_ref(),
    ))
}

/// A track timescale and frame duration for `fps`: the reduced fraction,
/// scaled towards a 90 kHz timescale. `None` if it cannot fit 32 bits.
pub fn track_timing(fps: Fps) -> Option<(u32, u32)> {
    let (mut a, mut b) = (fps.num, fps.den);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    let (num, den) = (fps.num / a, fps.den / a);
    let scale = (90_000 / num).max(1);
    let timescale = u32::try_from(num.checked_mul(scale)?).ok()?;
    let delta = u32::try_from(den.checked_mul(scale)?).ok()?;
    Some((timescale, delta))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_rates_become_track_timing() {
        let fps = |num, den| Fps { num, den };
        assert_eq!(track_timing(fps(30, 1)), Some((90_000, 3000)));
        assert_eq!(track_timing(fps(30000, 1001)), Some((90_000, 3003)));
        assert_eq!(track_timing(fps(60, 2)), Some((90_000, 3000)));
        assert_eq!(track_timing(fps(1_000_000, 7)), Some((1_000_000, 7)));
        assert_eq!(track_timing(fps(1, 5_000_000_000)), None);
    }
}
