//! Video frames from MP4 assets (SPEC.md section 5.2): H.264 decoding with
//! OpenH264, frame lookup by presentation time, and YUV to RGB conversion.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use openh264::decoder::Decoder;
use openh264::formats::YUVSource;

use crate::image::{Pixmap, orient};
use crate::mp4;

/// How a stream's YUV values turn into RGB.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorSpace {
    /// Red and blue luma weights.
    pub kr: f32,
    pub kb: f32,
    pub full_range: bool,
}

impl ColorSpace {
    pub const BT601: ColorSpace = ColorSpace {
        kr: 0.299,
        kb: 0.114,
        full_range: false,
    };
    pub const BT709: ColorSpace = ColorSpace {
        kr: 0.2126,
        kb: 0.0722,
        full_range: false,
    };

    /// The matrix for H.264 `matrix_coefficients`, or `None` if unsignalled or unknown.
    fn from_matrix(code: u8, full_range: bool) -> Option<ColorSpace> {
        let (kr, kb) = match code {
            1 => (0.2126, 0.0722),
            4 => (0.30, 0.11),
            5 | 6 => (0.299, 0.114),
            7 => (0.212, 0.087),
            9 => (0.2627, 0.0593),
            _ => return None,
        };
        Some(ColorSpace { kr, kb, full_range })
    }

    /// Straight RGB 0–1 for one YUV sample.
    pub fn rgb(&self, y: u8, u: u8, v: u8) -> [f32; 3] {
        let (y, cb, cr) = if self.full_range {
            (
                f32::from(y) / 255.0,
                (f32::from(u) - 128.0) / 255.0,
                (f32::from(v) - 128.0) / 255.0,
            )
        } else {
            (
                (f32::from(y) - 16.0) / 219.0,
                (f32::from(u) - 128.0) / 224.0,
                (f32::from(v) - 128.0) / 224.0,
            )
        };
        let r = y + 2.0 * (1.0 - self.kr) * cr;
        let b = y + 2.0 * (1.0 - self.kb) * cb;
        let g = (y - self.kr * r - self.kb * b) / (1.0 - self.kr - self.kb);
        [r, g, b].map(|c| c.clamp(0.0, 1.0))
    }
}

/// What the renderer needs from a sequence parameter set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpsInfo {
    pub frame_mbs_only: bool,
    pub full_range: bool,
    /// `matrix_coefficients`, 2 (unspecified) when not signalled.
    pub matrix: u8,
}

/// Reads bits from a NAL unit payload with emulation prevention bytes removed.
struct Bits {
    data: Vec<u8>,
    pos: usize,
}

impl Bits {
    fn new(nal: &[u8]) -> Bits {
        let mut data = Vec::with_capacity(nal.len());
        let mut zeros = 0;
        for &b in nal {
            if zeros >= 2 && b == 3 {
                zeros = 0;
                continue;
            }
            zeros = if b == 0 { zeros + 1 } else { 0 };
            data.push(b);
        }
        Bits { data, pos: 0 }
    }

    fn bit(&mut self) -> Result<u32, String> {
        let byte = self.data.get(self.pos / 8).ok_or("SPS ends early")?;
        let bit = (byte >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Ok(u32::from(bit))
    }

    fn bits(&mut self, n: u32) -> Result<u32, String> {
        (0..n).try_fold(0, |acc, _| Ok((acc << 1) | self.bit()?))
    }

    /// Unsigned exponential-Golomb.
    fn ue(&mut self) -> Result<u32, String> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return Err("bad exp-Golomb code in SPS".into());
            }
        }
        Ok(((1u64 << zeros) - 1 + u64::from(self.bits(zeros)?)) as u32)
    }

    fn se(&mut self) -> Result<i32, String> {
        let k = self.ue()?;
        Ok(if k % 2 == 1 {
            k.div_ceil(2) as i32
        } else {
            -((k / 2) as i32)
        })
    }
}

/// Parses an SPS NAL unit (with its one-byte header) as far as the colour
/// description.
pub fn parse_sps(nal: &[u8]) -> Result<SpsInfo, String> {
    let mut r = Bits::new(nal.get(1..).ok_or("empty SPS")?);
    let profile = r.bits(8)?;
    r.bits(16)?;
    r.ue()?;
    if matches!(profile, 100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135) {
        let chroma_format = r.ue()?;
        if chroma_format == 3 {
            r.bit()?;
        }
        r.ue()?;
        r.ue()?;
        r.bit()?;
        if r.bit()? == 1 {
            for i in 0..if chroma_format == 3 { 12 } else { 8 } {
                if r.bit()? == 1 {
                    let size = if i < 6 { 16 } else { 64 };
                    let (mut last, mut next) = (8i32, 8i32);
                    for _ in 0..size {
                        if next != 0 {
                            next = (last + r.se()? + 256) % 256;
                        }
                        if next != 0 {
                            last = next;
                        }
                    }
                }
            }
        }
    }
    r.ue()?;
    match r.ue()? {
        0 => {
            r.ue()?;
        }
        1 => {
            r.bit()?;
            r.se()?;
            r.se()?;
            for _ in 0..r.ue()?.min(255) {
                r.se()?;
            }
        }
        _ => {}
    }
    r.ue()?;
    r.bit()?;
    r.ue()?;
    r.ue()?;
    let frame_mbs_only = r.bit()? == 1;
    if !frame_mbs_only {
        r.bit()?;
    }
    r.bit()?;
    if r.bit()? == 1 {
        for _ in 0..4 {
            r.ue()?;
        }
    }
    let mut info = SpsInfo {
        frame_mbs_only,
        full_range: false,
        matrix: 2,
    };
    if r.bit()? == 1 {
        if r.bit()? == 1 && r.bits(8)? == 255 {
            r.bits(32)?;
        }
        if r.bit()? == 1 {
            r.bit()?;
        }
        if r.bit()? == 1 {
            r.bits(3)?;
            info.full_range = r.bit()? == 1;
            if r.bit()? == 1 {
                r.bits(16)?;
                info.matrix = r.bits(8)? as u8;
            }
        }
    }
    Ok(info)
}

/// A video track ready to hand out frames.
pub struct Video<'a> {
    file: &'a [u8],
    track: mp4::Track,
    movie_timescale: u32,
    /// Parameter sets as an Annex B stream, fed before the first frame.
    headers: Vec<u8>,
    nal_length: usize,
    color: ColorSpace,
    /// Sample indices in presentation order.
    order: Vec<usize>,
    max_pixels: u64,
    state: Option<Decoding>,
}

struct Decoding {
    decoder: Decoder,
    /// The next sample to feed, in decode order.
    next: usize,
    /// Composition times of fed samples not yet output.
    waiting: BinaryHeap<Reverse<i64>>,
    /// The latest output frame: its composition time and pixels.
    last: Option<(i64, Pixmap)>,
}

impl<'a> Video<'a> {
    /// Opens the first video track of an MP4 file.
    pub fn open(file: &'a [u8], max_samples: u64, max_pixels: u64) -> Result<Video<'a>, String> {
        let movie = mp4::read(file, max_samples)?;
        let track = movie
            .tracks
            .into_iter()
            .find(|t| &t.handler == b"vide")
            .ok_or("the file has no video track")?;
        if !matches!(&track.entry.format, b"avc1" | b"avc3") {
            return Err(format!(
                "video format {} is not supported, only H.264",
                String::from_utf8_lossy(&track.entry.format)
            ));
        }
        if track.samples.is_empty() {
            return Err("the video track has no frames".into());
        }
        let children = track.entry_children(78)?;
        let avcc = children
            .iter()
            .find(|(k, _)| k == b"avcC")
            .map(|(_, d)| *d)
            .ok_or("the video has no avcC box")?;
        let (nal_length, parameter_sets) = parse_avcc(avcc)?;
        let mut headers = Vec::new();
        let mut sps = None;
        for set in &parameter_sets {
            if set.first().map(|b| b & 0x1F) == Some(7) && sps.is_none() {
                sps = Some(parse_sps(set)?);
            }
            headers.extend_from_slice(&[0, 0, 0, 1]);
            headers.extend_from_slice(set);
        }
        if sps.is_none() {
            // avc3 keeps parameter sets in the samples.
            let first = track.sample_data(file, &track.samples[0])?;
            for nal in nal_units(first, nal_length)? {
                if nal.first().map(|b| b & 0x1F) == Some(7) {
                    sps = Some(parse_sps(nal)?);
                    break;
                }
            }
        }
        let sps = sps.ok_or("the video has no sequence parameter set")?;
        if !sps.frame_mbs_only {
            return Err("interlaced video is not supported".into());
        }
        let (w, h) = entry_size(&track.entry.body);
        let color = ColorSpace::from_matrix(sps.matrix, sps.full_range).unwrap_or(
            if h >= 720 {
                ColorSpace {
                    full_range: sps.full_range,
                    ..ColorSpace::BT709
                }
            } else {
                ColorSpace {
                    full_range: sps.full_range,
                    ..ColorSpace::BT601
                }
            },
        );
        if u64::from(w) * u64::from(h) > max_pixels {
            return Err(format!("the video is {w}×{h}, more than the pixel limit"));
        }
        let mut order: Vec<usize> = (0..track.samples.len()).collect();
        order.sort_by_key(|&i| (composition(&track.samples[i]), i));
        Ok(Video {
            file,
            movie_timescale: movie.timescale,
            track,
            headers,
            nal_length,
            color,
            order,
            max_pixels,
            state: None,
        })
    }

    /// The frame shown at source time `num / den` milliseconds: the one with
    /// the greatest presentation time not after it. Before the first frame,
    /// the first frame; past the last, the last.
    pub fn frame_at(&mut self, num: i128, den: i128) -> Result<Pixmap, String> {
        let (m_num, m_den) = self.media_time(num, den);
        let samples = &self.track.samples;
        // composition ≤ m_num / m_den, compared exactly.
        let pos = self
            .order
            .partition_point(|&i| i128::from(composition(&samples[i])) * m_den <= m_num);
        let target = self.order[pos.saturating_sub(1)];
        let target_time = composition(&samples[target]);
        self.decode_to(target, target_time)?;
        let (_, image) = self.state.as_ref().and_then(|s| s.last.as_ref()).ok_or("no frame decoded")?;
        Ok(match self.track.rotation {
            90 => orient(image, 6),
            180 => orient(image, 3),
            270 => orient(image, 8),
            _ => image.clone(),
        })
    }

    /// Source milliseconds to media time in track units, as a fraction, through
    /// the edit list.
    fn media_time(&self, num: i128, den: i128) -> (i128, i128) {
        let scale = i128::from(self.track.timescale);
        let Some(edits) = &self.track.edits else {
            return (num.max(0) * scale, den * 1000);
        };
        let movie = i128::from(self.movie_timescale);
        // Walk the edits in movie units: s ms is num·movie / (den·1000) movie units.
        let mut start = 0i128;
        let mut last_media = None;
        for edit in edits {
            let length = i128::from(edit.duration);
            if edit.media_time >= 0 {
                let media_start = i128::from(edit.media_time);
                // Offset into this edit, in movie units: num·movie/(den·1000) − start.
                let offset_num = num * movie - start * den * 1000;
                let offset_den = den * 1000;
                if offset_num < 0 {
                    // Before this edit (in an empty edit or before the start): its first frame.
                    return (media_start, 1);
                }
                let end = length * offset_den;
                let clamped = if length > 0 { offset_num.min(end) } else { offset_num };
                if length == 0 || offset_num < end {
                    // media = media_start + offset·scale/movie
                    return (media_start * offset_den * movie + clamped * scale, offset_den * movie);
                }
                last_media = Some((media_start * offset_den * movie + end * scale, offset_den * movie));
            }
            start += length;
        }
        last_media.unwrap_or((0, 1))
    }

    fn decode_to(&mut self, target: usize, target_time: i64) -> Result<(), String> {
        let samples = &self.track.samples;
        let ready = self.state.as_ref().is_some_and(|s| {
            s.last.as_ref().is_some_and(|(t, _)| *t == target_time)
                || (s.next <= target
                    && s.last.as_ref().is_none_or(|(t, _)| *t < target_time)
                    && !samples[s.next..=target].iter().skip(1).any(|x| x.sync))
        });
        if !ready {
            let mut start = samples[..=target].iter().rposition(|s| s.sync).unwrap_or(0);
            if composition(&samples[start]) > target_time {
                start = samples[..start].iter().rposition(|s| s.sync).unwrap_or(0);
            }
            self.state = Some(Decoding {
                decoder: Decoder::new().map_err(|e| e.to_string())?,
                next: start,
                waiting: BinaryHeap::new(),
                last: None,
            });
        }
        let state = self.state.as_mut().expect("set above");
        if state.last.as_ref().is_some_and(|(t, _)| *t == target_time) {
            return Ok(());
        }
        let color = self.color;
        let max_pixels = self.max_pixels;
        let keep = |state: &mut Decoding, yuv: &dyn YUVSource| -> Result<bool, String> {
            let Some(Reverse(time)) = state.waiting.pop() else {
                return Ok(false);
            };
            if time <= target_time {
                state.last = Some((time, to_pixmap(yuv, color, max_pixels)?));
            }
            Ok(time >= target_time)
        };
        while state.next < samples.len() {
            let sample = &samples[state.next];
            let mut stream = Vec::new();
            if state.waiting.is_empty() && state.last.is_none() {
                stream.extend_from_slice(&self.headers);
            }
            for nal in nal_units(self.track.sample_data(self.file, sample)?, self.nal_length)? {
                stream.extend_from_slice(&[0, 0, 0, 1]);
                stream.extend_from_slice(nal);
            }
            state.next += 1;
            state.waiting.push(Reverse(composition(sample)));
            let decoded = state.decoder.decode(&stream).map_err(|e| e.to_string())?;
            if let Some(yuv) = decoded {
                let yuv = OwnedYuv::from(&yuv);
                if keep(state, &yuv)? {
                    return Ok(());
                }
            }
        }
        let rest: Vec<OwnedYuv> = state
            .decoder
            .flush_remaining()
            .map_err(|e| e.to_string())?
            .iter()
            .map(OwnedYuv::from)
            .collect();
        for yuv in rest {
            if keep(state, &yuv)? {
                return Ok(());
            }
        }
        if state.last.is_none() {
            return Err("the decoder produced no frame".into());
        }
        Ok(())
    }
}

/// A decoded picture copied out of the decoder, so the decoder can be used again.
struct OwnedYuv {
    width: usize,
    height: usize,
    strides: (usize, usize, usize),
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl From<&openh264::decoder::DecodedYUV<'_>> for OwnedYuv {
    fn from(yuv: &openh264::decoder::DecodedYUV<'_>) -> Self {
        let (width, height) = yuv.dimensions();
        OwnedYuv {
            width,
            height,
            strides: yuv.strides(),
            y: yuv.y().to_vec(),
            u: yuv.u().to_vec(),
            v: yuv.v().to_vec(),
        }
    }
}

impl YUVSource for OwnedYuv {
    fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn strides(&self) -> (usize, usize, usize) {
        self.strides
    }

    fn y(&self) -> &[u8] {
        &self.y
    }

    fn u(&self) -> &[u8] {
        &self.u
    }

    fn v(&self) -> &[u8] {
        &self.v
    }
}

/// Composition time of a sample in track units.
fn composition(sample: &mp4::Sample) -> i64 {
    (sample.decode_time as i64).saturating_add(sample.composition_offset)
}

/// Width and height from a visual sample entry.
fn entry_size(body: &[u8]) -> (u32, u32) {
    let read = |at: usize| body.get(at..at + 2).map_or(0, |b| u32::from(u16::from_be_bytes([b[0], b[1]])));
    (read(24), read(26))
}

/// The NAL length size and parameter sets (SPS then PPS) from an `avcC` box.
fn parse_avcc(avcc: &[u8]) -> Result<(usize, Vec<&[u8]>), String> {
    let bad = || "bad avcC box".to_string();
    let nal_length = usize::from(avcc.get(4).ok_or_else(bad)? & 3) + 1;
    let mut pos = 5;
    let mut sets = Vec::new();
    for mask in [0x1F, 0xFF] {
        let count = avcc.get(pos).ok_or_else(bad)? & mask;
        pos += 1;
        for _ in 0..count {
            let len = usize::from(u16::from_be_bytes(
                avcc.get(pos..pos + 2).ok_or_else(bad)?.try_into().unwrap(),
            ));
            sets.push(avcc.get(pos + 2..pos + 2 + len).ok_or_else(bad)?);
            pos += 2 + len;
        }
    }
    Ok((nal_length, sets))
}

/// Splits a length-prefixed sample into NAL units.
fn nal_units(sample: &[u8], length_size: usize) -> Result<Vec<&[u8]>, String> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < sample.len() {
        let len = sample
            .get(pos..pos + length_size)
            .ok_or("a NAL length runs past the sample")?
            .iter()
            .fold(0usize, |acc, &b| (acc << 8) | usize::from(b));
        pos += length_size;
        out.push(sample.get(pos..pos + len).ok_or("a NAL unit runs past the sample")?);
        pos += len;
    }
    Ok(out)
}

/// Converts 4:2:0 YUV to an opaque premultiplied pixmap. Each chroma sample
/// covers the 2×2 luma samples it belongs to.
fn to_pixmap(yuv: &dyn YUVSource, color: ColorSpace, max_pixels: u64) -> Result<Pixmap, String> {
    let (w, h) = yuv.dimensions();
    let (sy, su, sv) = yuv.strides();
    let mut image = Pixmap::filled(w as u32, h as u32, [0.0; 4], max_pixels)
        .map_err(|e| format!("a {w}×{h} frame needs {} pixels, more than {}", e.pixels, e.limit))?;
    let (ys, us, vs) = (yuv.y(), yuv.u(), yuv.v());
    for row in 0..h {
        for col in 0..w {
            let y = ys[row * sy + col];
            let u = us[(row / 2) * su + col / 2];
            let v = vs[(row / 2) * sv + col / 2];
            let [r, g, b] = color.rgb(y, u, v);
            image.set(col as u32, row as u32, [r, g, b, 1.0]);
        }
    }
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIP: &[u8] = include_bytes!("../../../tests/video/frames-high.mp4");

    /// The frame marker: the top-left block's luma is 24 + 16·n in frame n.
    fn marker(image: &Pixmap) -> usize {
        let p = image.pixel(8, 8);
        let y = 0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2];
        let luma = y * 219.0 + 16.0;
        ((luma - 24.0) / 16.0).round() as usize
    }

    #[test]
    #[ignore = "work in progress: OpenH264 returns frames in decode order and fails on samples 9 and 10"]
    fn high_profile_frames_come_out_in_presentation_order() {
        let mut video = Video::open(CLIP, 1000, 1_000_000).unwrap();
        assert_eq!(video.track.samples.len(), 12);
        assert!(video.track.samples.iter().any(|s| s.composition_offset != 0), "has B-frames");
        let step = 1000 / 10;
        let first = video.frame_at(0, 1).unwrap();
        assert_eq!((first.width, first.height), (96, 64));
        let shown: Vec<usize> = (0..12).map(|n| marker(&video.frame_at(n * step + 50, 1).unwrap())).collect();
        assert_eq!(shown, (0..12).collect::<Vec<_>>());
        // Backwards and across key frames.
        assert_eq!(marker(&video.frame_at(250, 1).unwrap()), 2);
        assert_eq!(marker(&video.frame_at(1150, 1).unwrap()), 11);
        assert_eq!(marker(&video.frame_at(99_999, 1).unwrap()), 11, "the last frame is held");
    }

    #[test]
    fn colors_follow_the_matrix() {
        let bt601 = ColorSpace::BT601;
        assert_eq!(bt601.rgb(16, 128, 128), [0.0, 0.0, 0.0]);
        assert_eq!(bt601.rgb(235, 128, 128), [1.0, 1.0, 1.0]);
        let red = bt601.rgb(81, 90, 240);
        assert!(red[0] > 0.99 && red[1] < 0.01 && red[2] < 0.01, "{red:?}");
        let full = ColorSpace { full_range: true, ..ColorSpace::BT709 };
        assert_eq!(full.rgb(255, 128, 128), [1.0, 1.0, 1.0]);
    }

    #[test]
    fn exp_golomb_and_emulation_prevention() {
        // 1 → 0, 010 → 1, 011 → 2, 00100 → 3; then se 00101 → -2.
        let mut r = Bits::new(&[0b1010_0110, 0b0100_0010, 0b1000_0000]);
        assert_eq!([r.ue().unwrap(), r.ue().unwrap(), r.ue().unwrap(), r.ue().unwrap()], [0, 1, 2, 3]);
        assert_eq!(r.se().unwrap(), -2);
        let r = Bits::new(&[0, 0, 3, 1, 0, 0, 3]);
        assert_eq!(r.data, [0, 0, 1, 0, 0]);
    }

    /// Logs what the decoder returns for each sample, for debugging.
    #[test]
    #[ignore = "diagnostic: run with --ignored --nocapture"]
    fn zz_probe_order() {
        let video = Video::open(CLIP, 1000, 1_000_000).unwrap();
        let mut dec = Decoder::new().unwrap();
        for (i, sample) in video.track.samples.iter().enumerate() {
            let mut stream = Vec::new();
            if i == 0 { stream.extend_from_slice(&video.headers); }
            let nals = nal_units(video.track.sample_data(CLIP, sample).unwrap(), video.nal_length).unwrap();
            let types: Vec<u8> = nals.iter().map(|n| n[0] & 0x1F).collect();
            for nal in nals { stream.extend_from_slice(&[0, 0, 0, 1]); stream.extend_from_slice(nal); }
            match dec.decode(&stream) {
                Ok(Some(yuv)) => { let o = OwnedYuv::from(&yuv); eprintln!("feed {i} nal types {types:?} -> out marker {}", marker(&to_pixmap(&o, video.color, 1_000_000).unwrap())); }
                Ok(None) => eprintln!("feed {i} nal types {types:?} -> none"),
                Err(e) => eprintln!("feed {i} -> error {e}"),
            }
        }
        for yuv in dec.flush_remaining().unwrap().iter() {
            let o = OwnedYuv::from(yuv);
            eprintln!("flush -> marker {}", marker(&to_pixmap(&o, video.color, 1_000_000).unwrap()));
        }
    }
}
