//! Video frames from MP4 assets (SPEC.md section 5.2): H.264 decoding with
//! OpenH264, frame lookup by presentation time, and YUV to RGB conversion.

use openh264::decoder::Decoder;
use openh264::formats::YUVSource;
use openh264_sys2::SBufferInfo;

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
    if matches!(
        profile,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
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
    /// The latest frame handed out: its sample index and pixels.
    last: Option<(usize, Pixmap)>,
}

/// Most decoded pictures kept waiting for their turn. H.264 never holds more
/// than 16 in its picture buffer.
const MAX_PENDING: usize = 16;

struct Decoding {
    decoder: Decoder,
    /// The next sample to feed, in decode order.
    next: usize,
    /// Whether the end of the stream has been signalled.
    ended: bool,
    /// Pictures that came out before they were asked for, by sample index.
    pending: Vec<(usize, OwnedYuv)>,
    /// Samples whose pictures came out and were let go.
    dropped: Vec<bool>,
    /// The latest error OpenH264 reported.
    error: Option<i32>,
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
        let color = ColorSpace::from_matrix(sps.matrix, sps.full_range).unwrap_or(if h >= 720 {
            ColorSpace {
                full_range: sps.full_range,
                ..ColorSpace::BT709
            }
        } else {
            ColorSpace {
                full_range: sps.full_range,
                ..ColorSpace::BT601
            }
        });
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
            last: None,
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
        if self.last.as_ref().is_none_or(|(i, _)| *i != target) {
            let yuv = self.decode(target)?;
            self.last = Some((target, to_pixmap(&yuv, self.color, self.max_pixels)?));
        }
        let (_, image) = self.last.as_ref().expect("set above");
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
                let clamped = if length > 0 {
                    offset_num.min(end)
                } else {
                    offset_num
                };
                if length == 0 || offset_num < end {
                    // media = media_start + offset·scale/movie
                    return (
                        media_start * offset_den * movie + clamped * scale,
                        offset_den * movie,
                    );
                }
                last_media = Some((
                    media_start * offset_den * movie + end * scale,
                    offset_den * movie,
                ));
            }
            start += length;
        }
        last_media.unwrap_or((0, 1))
    }

    /// The key frame to start decoding from to reach `target`.
    fn start_for(&self, target: usize) -> usize {
        let samples = &self.track.samples;
        let start = samples[..=target].iter().rposition(|s| s.sync).unwrap_or(0);
        if composition(&samples[start]) > composition(&samples[target]) {
            // An open group of pictures: the target is shown before its key
            // frame and may refer back to the group before.
            samples[..start].iter().rposition(|s| s.sync).unwrap_or(0)
        } else {
            start
        }
    }

    /// Decodes the picture of sample `target`. Pictures come out of OpenH264
    /// in its own order; each carries the tag it was fed with, so the order
    /// never decides which picture is which.
    fn decode(&mut self, target: usize) -> Result<OwnedYuv, String> {
        let samples = &self.track.samples;
        let start = self.start_for(target);
        let reuse = self.state.as_ref().is_some_and(|s| {
            s.pending.iter().any(|(i, _)| *i == target) || (!s.dropped[target] && start <= s.next)
        });
        if !reuse {
            let mut decoder = Decoder::new().map_err(|e| e.to_string())?;
            // An empty feed would signal the end of the stream.
            let error = if self.headers.is_empty() {
                None
            } else {
                feed(&mut decoder, Some(&self.headers), 0)?.0
            };
            self.state = Some(Decoding {
                decoder,
                next: start,
                ended: false,
                pending: Vec::new(),
                dropped: vec![false; samples.len()],
                error,
            });
        }
        let state = self.state.as_mut().expect("set above");
        let time = composition(&samples[target]);
        loop {
            if let Some(at) = state.pending.iter().position(|(i, _)| *i == target) {
                let (_, yuv) = state.pending.swap_remove(at);
                state.dropped[target] = true;
                return Ok(yuv);
            }
            let (error, picture) = if state.next < samples.len() {
                let mut stream = Vec::new();
                for nal in nal_units(
                    self.track.sample_data(self.file, &samples[state.next])?,
                    self.nal_length,
                )? {
                    stream.extend_from_slice(&[0, 0, 0, 1]);
                    stream.extend_from_slice(nal);
                }
                state.next += 1;
                feed(&mut state.decoder, Some(&stream), state.next as u64)?
            } else if !state.ended {
                state.ended = true;
                feed(&mut state.decoder, None, 0)?
            } else {
                match flush(&mut state.decoder) {
                    Some(picture) => (None, Some(picture)),
                    None => {
                        let why = state
                            .error
                            .map_or(String::new(), |code| format!(" (OpenH264 error {code:#x})"));
                        return Err(format!("frame {target} could not be decoded{why}"));
                    }
                }
            };
            state.error = error.or(state.error);
            // Tags are sample index + 1; 0 marks the parameter sets.
            let Some((index, yuv)) = picture.and_then(|(tag, yuv)| {
                let index = usize::try_from(tag).ok()?.checked_sub(1)?;
                (index < samples.len()).then_some((index, yuv))
            }) else {
                continue;
            };
            state.dropped[index] = true;
            // Pictures shown before the target are not needed going forward.
            if composition(&samples[index]) >= time
                && !state.pending.iter().any(|(i, _)| *i == index)
            {
                state.dropped[index] = false;
                state.pending.push((index, yuv));
                if state.pending.len() > MAX_PENDING {
                    let (at, _) = state
                        .pending
                        .iter()
                        .enumerate()
                        .max_by_key(|(_, (i, _))| composition(&samples[*i]))
                        .expect("not empty");
                    let (i, _) = state.pending.swap_remove(at);
                    state.dropped[i] = true;
                }
            }
        }
    }
}

/// Feeds one access unit to OpenH264 tagged with `tag`, or `None` to mark the
/// end of the stream. Returns the error code, if any, and the picture that
/// came out with its tag.
#[allow(unsafe_code)]
fn feed(
    decoder: &mut Decoder,
    stream: Option<&[u8]>,
    tag: u64,
) -> Result<(Option<i32>, Option<Tagged>), String> {
    let (data, len) = match stream {
        Some(s) => (
            s.as_ptr(),
            i32::try_from(s.len()).map_err(|_| "a video frame is too large")?,
        ),
        None => (std::ptr::null(), 0),
    };
    let mut info = SBufferInfo {
        uiInBsTimeStamp: tag,
        ..SBufferInfo::default()
    };
    let mut dst = [std::ptr::null_mut::<u8>(); 3];
    // SAFETY: `data` points to `len` readable bytes or is null with length 0,
    // which OpenH264 takes as end of stream; `dst` and `info` are live locals.
    let state = unsafe {
        decoder
            .raw_api()
            .decode_frame2(data, len, dst.as_mut_ptr(), &raw mut info)
    };
    Ok(((state != 0).then_some(state), picture(&info)))
}

/// Takes the next buffered picture after the end of the stream.
#[allow(unsafe_code)]
fn flush(decoder: &mut Decoder) -> Option<Tagged> {
    let mut info = SBufferInfo::default();
    let mut dst = [std::ptr::null_mut::<u8>(); 3];
    // SAFETY: `dst` and `info` are live locals.
    unsafe {
        decoder
            .raw_api()
            .flush_frame(dst.as_mut_ptr(), &raw mut info)
    };
    picture(&info)
}

/// Copies the picture OpenH264 reported in `info`, if there is one.
#[allow(unsafe_code)]
fn picture(info: &SBufferInfo) -> Option<Tagged> {
    if info.iBufferStatus != 1 || info.pDst.iter().any(|p| p.is_null()) {
        return None;
    }
    // SAFETY: with a picture ready, OpenH264 fills the system buffer description.
    let buffer = unsafe { info.UsrData.sSystemBuffer };
    let size = |v: i32| usize::try_from(v).ok().filter(|&v| v > 0);
    let (width, height) = (size(buffer.iWidth)?, size(buffer.iHeight)?);
    let (luma, chroma) = (size(buffer.iStride[0])?, size(buffer.iStride[1])?);
    if luma < width || chroma < width.div_ceil(2) {
        return None;
    }
    let rows = height.div_ceil(2);
    // SAFETY: the planes stay valid until the next call into the decoder, and
    // are at least stride × rows long (OpenH264 also pads them). Each is
    // copied before returning.
    let (y, u, v) = unsafe {
        (
            std::slice::from_raw_parts(info.pDst[0], luma * height).to_vec(),
            std::slice::from_raw_parts(info.pDst[1], chroma * rows).to_vec(),
            std::slice::from_raw_parts(info.pDst[2], chroma * rows).to_vec(),
        )
    };
    Some((
        info.uiOutYuvTimeStamp,
        OwnedYuv {
            width,
            height,
            strides: (luma, chroma, chroma),
            y,
            u,
            v,
        },
    ))
}

/// A picture and the tag it was fed with.
type Tagged = (u64, OwnedYuv);

/// A decoded picture copied out of the decoder, so the decoder can be used again.
struct OwnedYuv {
    width: usize,
    height: usize,
    strides: (usize, usize, usize),
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
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
    let read = |at: usize| {
        body.get(at..at + 2)
            .map_or(0, |b| u32::from(u16::from_be_bytes([b[0], b[1]])))
    };
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
        out.push(
            sample
                .get(pos..pos + len)
                .ok_or("a NAL unit runs past the sample")?,
        );
        pos += len;
    }
    Ok(out)
}

/// Converts 4:2:0 YUV to an opaque premultiplied pixmap. Each chroma sample
/// covers the 2×2 luma samples it belongs to.
fn to_pixmap(yuv: &dyn YUVSource, color: ColorSpace, max_pixels: u64) -> Result<Pixmap, String> {
    let (w, h) = yuv.dimensions();
    let (sy, su, sv) = yuv.strides();
    let mut image = Pixmap::filled(w as u32, h as u32, [0.0; 4], max_pixels).map_err(|e| {
        format!(
            "a {w}×{h} frame needs {} pixels, more than {}",
            e.pixels, e.limit
        )
    })?;
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
    fn high_profile_frames_come_out_in_presentation_order() {
        let mut video = Video::open(CLIP, 1000, 1_000_000).unwrap();
        // VLC's x264 encode: 11 frames at 90 kHz, frame 0 encoded twice and
        // nothing shown at 100 ms. B-frames make decode order differ from
        // presentation order.
        assert_eq!(video.track.samples.len(), 11);
        assert_eq!(video.order, [0, 2, 3, 1, 5, 4, 6, 7, 8, 9, 10]);
        let first = video.frame_at(0, 1).unwrap();
        assert_eq!((first.width, first.height), (96, 64));
        let shown: Vec<usize> = (0..12)
            .map(|n| marker(&video.frame_at(n * 100 + 50, 1).unwrap()))
            .collect();
        assert_eq!(shown, [0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
        // Backwards, then across the key frame at sample 6, then back again.
        assert_eq!(marker(&video.frame_at(450, 1).unwrap()), 2);
        assert_eq!(marker(&video.frame_at(750, 1).unwrap()), 5);
        assert_eq!(marker(&video.frame_at(650, 1).unwrap()), 4);
        assert_eq!(marker(&video.frame_at(1, 3).unwrap()), 0);
        assert_eq!(
            marker(&video.frame_at(99_999, 1).unwrap()),
            9,
            "the last frame is held"
        );
    }

    #[test]
    fn damaged_frames_are_an_error_not_a_panic() {
        let mut clip = CLIP.to_vec();
        let video = Video::open(CLIP, 1000, 1_000_000).unwrap();
        let sample = &video.track.samples[3];
        let start = sample.offset as usize;
        for byte in &mut clip[start + 8..start + sample.size as usize] {
            *byte = 0xFF;
        }
        let mut video = Video::open(&clip, 1000, 1_000_000).unwrap();
        for n in 0..12 {
            let _ = video.frame_at(n * 100 + 50, 1);
        }
        assert_eq!(
            marker(&video.frame_at(50, 1).unwrap()),
            0,
            "frames before the damage still decode"
        );
    }

    #[test]
    fn colors_follow_the_matrix() {
        let bt601 = ColorSpace::BT601;
        assert_eq!(bt601.rgb(16, 128, 128), [0.0, 0.0, 0.0]);
        assert_eq!(bt601.rgb(235, 128, 128), [1.0, 1.0, 1.0]);
        let red = bt601.rgb(81, 90, 240);
        assert!(red[0] > 0.99 && red[1] < 0.01 && red[2] < 0.01, "{red:?}");
        let full = ColorSpace {
            full_range: true,
            ..ColorSpace::BT709
        };
        assert_eq!(full.rgb(255, 128, 128), [1.0, 1.0, 1.0]);
    }

    #[test]
    fn exp_golomb_and_emulation_prevention() {
        // 1 → 0, 010 → 1, 011 → 2, 00100 → 3; then se 00101 → -2.
        let mut r = Bits::new(&[0b1010_0110, 0b0100_0010, 0b1000_0000]);
        assert_eq!(
            [
                r.ue().unwrap(),
                r.ue().unwrap(),
                r.ue().unwrap(),
                r.ue().unwrap()
            ],
            [0, 1, 2, 3]
        );
        assert_eq!(r.se().unwrap(), -2);
        let r = Bits::new(&[0, 0, 3, 1, 0, 0, 3]);
        assert_eq!(r.data, [0, 0, 1, 0, 0]);
    }
}
