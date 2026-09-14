//! Rendering hostile recipes before encoding: stills, the first video frames,
//! and the sound mix. Assets come from a fixed set of small files, and the
//! limits are small so a recipe can only ask for a little work.

#![no_main]

use std::collections::BTreeMap;
use std::sync::LazyLock;

use libfuzzer_sys::fuzz_target;
use unbaked_core::recipe::{self, OutputKind};
use unbaked_core::{rules, sha256_hex, sniff};
use unbaked_render::scene::{Frames, render_still};
use unbaked_render::timing::{Moment, frame_count};
use unbaked_render::{FontSource, RenderLimits, image, sound};

const CLIP: &[u8] = include_bytes!("../../tests/video/frames-high.mp4");
const LATO: &[u8] = include_bytes!("../../tests/fonts/Lato-Regular.ttf");
const OFL: &[u8] = include_bytes!("../../tests/fonts/OFL.txt");
const SOUND: &[u8] = include_bytes!("../seeds/files/tiny.unbaked.m4a");

/// The package every recipe renders against.
static FILES: LazyLock<BTreeMap<&'static str, Vec<u8>>> = LazyLock::new(|| {
    // A 3x2 picture with every alpha from clear to opaque.
    let rgba: Vec<u8> = (0..6u8)
        .flat_map(|i| [i * 40, 255 - i * 40, 128, i * 51])
        .collect();
    let png = image::encode_png(3, 2, &rgba).expect("a small PNG encodes");
    BTreeMap::from([
        ("assets/image.png", png),
        ("assets/clip.mp4", CLIP.to_vec()),
        ("assets/sound.m4a", SOUND.to_vec()),
        ("assets/fonts/Lato-Regular.ttf", LATO.to_vec()),
        ("assets/fonts/OFL.txt", OFL.to_vec()),
    ])
});

/// Referenced fonts: Lato, found by its fingerprint.
struct Lato;

impl FontSource for Lato {
    fn find(&self, sha256: &str) -> Option<Vec<u8>> {
        (sha256 == sha256_hex(LATO)).then(|| LATO.to_vec())
    }
}

fuzz_target!(|json: &[u8]| {
    let Ok(recipe) = recipe::parse(json) else {
        return;
    };
    let files = &*FILES;
    let kind_of = |path: &str| {
        files
            .get(path)
            .map(|d| sniff::detect(&d[..d.len().min(sniff::HEADER_LEN)]))
    };
    if !rules::check(&recipe, &kind_of).is_empty() {
        return;
    }
    let file = |path: &str| files.get(path).map(Vec::as_slice);
    let limits = RenderLimits {
        max_pixels: 1 << 16,
        max_samples: 1 << 17,
        max_frames: 1 << 16,
        deadline: None,
    };
    match recipe.output.kind {
        OutputKind::Image => {
            if let Ok(canvas) = render_still(&recipe, &file, &Lato, limits) {
                let _ = canvas.to_rgba8();
            }
        }
        OutputKind::Video => {
            let (Some(fps), Some(duration_ms)) = (recipe.output.fps, recipe.output.duration_ms)
            else {
                return;
            };
            let Ok(mut frames) = Frames::new(&recipe, &file, &Lato, limits) else {
                return;
            };
            // The first and last frames.
            let last = frame_count(duration_ms, fps).saturating_sub(1);
            for n in [0, last] {
                if frames.draw(Moment::Frame { n, fps }).is_err() {
                    break;
                }
            }
            let _ = sound::mix(&recipe, &file, limits);
        }
        OutputKind::Audio => {
            let _ = sound::mix(&recipe, &file, limits);
        }
    }
});
