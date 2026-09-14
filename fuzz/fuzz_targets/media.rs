//! The renderer's readers for hostile asset files: the MP4 reader, video
//! frames through OpenH264, and sound decoding.

#![no_main]

use libfuzzer_sys::fuzz_target;
use unbaked_render::{mp4, sound, video::Video};

const MAX_SAMPLES: u64 = 1 << 16;
const MAX_PIXELS: u64 = 1 << 20;

fuzz_target!(|file: &[u8]| {
    let _ = mp4::read(file, MAX_SAMPLES);
    if let Ok(mut video) = Video::open(file, MAX_SAMPLES, MAX_PIXELS) {
        // The first frame, one a little later, and one past the end.
        for ms in [0, 40, 1_000_000] {
            if let Ok(frame) = video.frame_at(ms, 1) {
                assert_eq!(
                    frame.data.len() as u64,
                    u64::from(frame.width) * u64::from(frame.height) * 4
                );
            }
        }
    }
    let _ = sound::decode(file, MAX_SAMPLES);
});
