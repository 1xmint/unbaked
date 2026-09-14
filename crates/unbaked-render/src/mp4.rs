//! Reading and writing MP4 files (ISO/IEC 14496-12 and 14496-14): enough to
//! take audio and video samples out of assets and to write rendered output.
//!
//! Fragmented files are not supported. Every count read from a file is checked
//! against the bytes that hold it before anything is allocated.

/// A parsed movie: its tracks and their sample tables.
#[derive(Debug, Clone, PartialEq)]
pub struct Movie {
    /// Units per second of edit list durations.
    pub timescale: u32,
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    /// `soun`, `vide`, and so on.
    pub handler: [u8; 4],
    /// Units per second of sample times.
    pub timescale: u32,
    /// The first sample description.
    pub entry: SampleEntry,
    pub samples: Vec<Sample>,
    /// The edit list, if the track has one.
    pub edits: Option<Vec<Edit>>,
    /// Clockwise display rotation from the track header: 0, 90, 180 or 270.
    pub rotation: u16,
}

/// A sample description: its format (`mp4a`, `avc1`, ...) and the bytes after
/// the box header.
#[derive(Debug, Clone, PartialEq)]
pub struct SampleEntry {
    pub format: [u8; 4],
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    /// Byte offset of the sample in the file.
    pub offset: u64,
    pub size: u32,
    /// Decode time in track timescale units.
    pub decode_time: u64,
    /// Composition time minus decode time.
    pub composition_offset: i64,
    /// A sync sample (a key frame).
    pub sync: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edit {
    /// Length in movie timescale units.
    pub duration: u64,
    /// Where the edit starts in the media, in track timescale units, or `-1`
    /// for an empty edit.
    pub media_time: i64,
}

impl Track {
    /// The bytes of `sample` in `file`.
    pub fn sample_data<'f>(&self, file: &'f [u8], sample: &Sample) -> Result<&'f [u8], String> {
        let start = usize::try_from(sample.offset).map_err(|_| "sample offset too large")?;
        start
            .checked_add(sample.size as usize)
            .and_then(|end| file.get(start..end))
            .ok_or_else(|| "a sample lies outside the file".into())
    }

    /// Child boxes of the sample entry, after `fixed` bytes of fields.
    pub fn entry_children(&self, fixed: usize) -> Result<Vec<Child<'_>>, String> {
        boxes(
            self.entry
                .body
                .get(fixed..)
                .ok_or("sample entry too short")?,
        )
    }
}

/// A box: its type and payload.
pub type Child<'a> = ([u8; 4], &'a [u8]);

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).ok_or("box too short")?;
        let out = self.data.get(self.pos..end).ok_or("box too short")?;
        self.pos = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_be_bytes(self.bytes(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes(self.bytes(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_be_bytes(self.bytes(8)?.try_into().unwrap()))
    }

    /// Version and flags of a full box.
    fn full(&mut self) -> Result<(u8, u32), String> {
        let v = self.u32()?;
        Ok(((v >> 24) as u8, v & 0x00FF_FFFF))
    }

    /// A table entry count, checked against the bytes left.
    fn count(&mut self, entry_len: usize) -> Result<usize, String> {
        let n = self.u32()? as usize;
        if n.saturating_mul(entry_len) > self.data.len() - self.pos {
            return Err("a table claims more entries than its box holds".into());
        }
        Ok(n)
    }
}

/// Splits `data` into boxes: `(type, payload)`.
pub fn boxes(data: &[u8]) -> Result<Vec<Child<'_>>, String> {
    let mut out = Vec::new();
    let mut r = Reader::new(data);
    while r.pos < data.len() {
        let start = r.pos;
        let size = r.u32()?;
        let kind: [u8; 4] = r.bytes(4)?.try_into().unwrap();
        let len = match size {
            0 => (data.len() - start) as u64,
            1 => r.u64()?,
            n => u64::from(n),
        };
        let header = r.pos - start;
        let len = usize::try_from(len).map_err(|_| "box too large")?;
        if len < header || len > data.len() - start {
            return Err(format!(
                "box {} has a bad size",
                String::from_utf8_lossy(&kind)
            ));
        }
        out.push((kind, &data[r.pos..start + len]));
        r.pos = start + len;
    }
    Ok(out)
}

fn child<'a>(children: &[Child<'a>], kind: &[u8; 4]) -> Option<&'a [u8]> {
    children.iter().find(|(k, _)| k == kind).map(|(_, d)| *d)
}

fn need<'a>(children: &[Child<'a>], kind: &[u8; 4]) -> Result<&'a [u8], String> {
    child(children, kind).ok_or_else(|| format!("missing {} box", String::from_utf8_lossy(kind)))
}

/// Timescale and duration from `mvhd` or `mdhd`.
fn header_timescale(data: &[u8]) -> Result<u32, String> {
    let mut r = Reader::new(data);
    let (version, _) = r.full()?;
    r.bytes(if version == 1 { 16 } else { 8 })?;
    let timescale = r.u32()?;
    if timescale == 0 {
        return Err("timescale is 0".into());
    }
    Ok(timescale)
}

/// Reads the movie structure of an MP4 file. `max_samples` caps the samples
/// per track.
pub fn read(file: &[u8], max_samples: u64) -> Result<Movie, String> {
    let top = boxes(file)?;
    if child(&top, b"moof").is_some() {
        return Err("fragmented MP4 files are not supported".into());
    }
    let moov = boxes(need(&top, b"moov")?)?;
    let timescale = header_timescale(need(&moov, b"mvhd")?)?;
    let mut tracks = Vec::new();
    for (_, trak) in moov.iter().filter(|(k, _)| k == b"trak") {
        tracks.push(read_track(trak, max_samples)?);
    }
    Ok(Movie { timescale, tracks })
}

fn read_track(trak: &[u8], max_samples: u64) -> Result<Track, String> {
    let trak = boxes(trak)?;
    let rotation = read_rotation(need(&trak, b"tkhd")?)?;
    let edits = match child(&trak, b"edts").map(boxes).transpose()? {
        Some(edts) => match child(&edts, b"elst") {
            Some(elst) => Some(read_edits(elst)?),
            None => None,
        },
        None => None,
    };
    let mdia = boxes(need(&trak, b"mdia")?)?;
    let timescale = header_timescale(need(&mdia, b"mdhd")?)?;
    let mut hdlr = Reader::new(need(&mdia, b"hdlr")?);
    hdlr.full()?;
    hdlr.u32()?;
    let handler: [u8; 4] = hdlr.bytes(4)?.try_into().unwrap();
    let minf = boxes(need(&mdia, b"minf")?)?;
    let stbl = boxes(need(&minf, b"stbl")?)?;

    let mut stsd = Reader::new(need(&stbl, b"stsd")?);
    stsd.full()?;
    if stsd.u32()? == 0 {
        return Err("the track has no sample description".into());
    }
    let entry_len = stsd.u32()? as usize;
    let format: [u8; 4] = stsd.bytes(4)?.try_into().unwrap();
    let body = stsd.bytes(entry_len.checked_sub(8).ok_or("bad sample description")?)?;
    let entry = SampleEntry {
        format,
        body: body.to_vec(),
    };

    let sizes = read_sizes(&stbl, max_samples)?;
    let n = sizes.len();
    let offsets = read_offsets(&stbl, &sizes)?;
    let times = read_times(need(&stbl, b"stts")?, n)?;
    let composition = match child(&stbl, b"ctts") {
        Some(ctts) => read_composition(ctts, n)?,
        None => vec![0; n],
    };
    let mut sync = vec![child(&stbl, b"stss").is_none(); n];
    if let Some(stss) = child(&stbl, b"stss") {
        let mut r = Reader::new(stss);
        r.full()?;
        for _ in 0..r.count(4)? {
            let i = r.u32()? as usize;
            if (1..=n).contains(&i) {
                sync[i - 1] = true;
            }
        }
    }
    let samples = (0..n)
        .map(|i| Sample {
            offset: offsets[i],
            size: sizes[i],
            decode_time: times[i],
            composition_offset: composition[i],
            sync: sync[i],
        })
        .collect();
    Ok(Track {
        handler,
        rotation,
        timescale,
        entry,
        samples,
        edits,
    })
}

/// The rotation in a `tkhd` matrix. Anything but a quarter turn is ignored.
fn read_rotation(tkhd: &[u8]) -> Result<u16, String> {
    let mut r = Reader::new(tkhd);
    let (version, _) = r.full()?;
    r.bytes(if version == 1 { 32 } else { 20 })?;
    r.bytes(16)?;
    let [a, b, _, c, d] = [r.u32()?, r.u32()?, r.u32()?, r.u32()?, r.u32()?].map(|v| v as i32);
    const ONE: i32 = 0x1_0000;
    Ok(if (a, b, c, d) == (0, ONE, -ONE, 0) {
        90
    } else if (a, b, c, d) == (-ONE, 0, 0, -ONE) {
        180
    } else if (a, b, c, d) == (0, -ONE, ONE, 0) {
        270
    } else {
        0
    })
}

fn read_edits(elst: &[u8]) -> Result<Vec<Edit>, String> {
    let mut r = Reader::new(elst);
    let (version, _) = r.full()?;
    let count = r.count(if version == 1 { 20 } else { 12 })?;
    let mut edits = Vec::with_capacity(count);
    for _ in 0..count {
        let (duration, media_time) = if version == 1 {
            (r.u64()?, r.u64()? as i64)
        } else {
            (u64::from(r.u32()?), i64::from(r.u32()? as i32))
        };
        let (rate, fraction) = (r.u16()?, r.u16()?);
        if media_time >= 0 && (rate, fraction) != (1, 0) {
            return Err("edit lists with a rate other than 1 are not supported".into());
        }
        if media_time < -1 {
            return Err("an edit has a negative media time".into());
        }
        edits.push(Edit {
            duration,
            media_time,
        });
    }
    Ok(edits)
}

fn read_sizes(stbl: &[Child<'_>], max_samples: u64) -> Result<Vec<u32>, String> {
    let too_many = |n: usize| {
        if n as u64 > max_samples {
            Err(format!(
                "the track has {n} samples, more than the limit of {max_samples}"
            ))
        } else {
            Ok(())
        }
    };
    if let Some(stsz) = child(stbl, b"stsz") {
        let mut r = Reader::new(stsz);
        r.full()?;
        let fixed = r.u32()?;
        if fixed != 0 {
            let n = r.u32()? as usize;
            too_many(n)?;
            return Ok(vec![fixed; n]);
        }
        let n = r.count(4)?;
        too_many(n)?;
        return (0..n).map(|_| r.u32()).collect();
    }
    let stz2 = need(stbl, b"stz2")?;
    let mut r = Reader::new(stz2);
    r.full()?;
    r.bytes(3)?;
    let bits = r.u8()?;
    let n = r.u32()? as usize;
    too_many(n)?;
    let bytes_needed = match bits {
        4 => n.div_ceil(2),
        8 => n,
        16 => n.saturating_mul(2),
        _ => return Err("bad stz2 field size".into()),
    };
    let data = r.bytes(bytes_needed)?;
    Ok((0..n)
        .map(|i| match bits {
            4 => u32::from((data[i / 2] >> if i % 2 == 0 { 4 } else { 0 }) & 0xF),
            8 => u32::from(data[i]),
            _ => u32::from(u16::from_be_bytes([data[2 * i], data[2 * i + 1]])),
        })
        .collect())
}

fn read_offsets(stbl: &[Child<'_>], sizes: &[u32]) -> Result<Vec<u64>, String> {
    let chunks: Vec<u64> = if let Some(stco) = child(stbl, b"stco") {
        let mut r = Reader::new(stco);
        r.full()?;
        (0..r.count(4)?)
            .map(|_| r.u32().map(u64::from))
            .collect::<Result<_, _>>()?
    } else {
        let mut r = Reader::new(need(stbl, b"co64")?);
        r.full()?;
        (0..r.count(8)?)
            .map(|_| r.u64())
            .collect::<Result<_, _>>()?
    };
    let mut r = Reader::new(need(stbl, b"stsc")?);
    r.full()?;
    let runs: Vec<(u32, u32)> = (0..r.count(12)?)
        .map(|_| {
            let first = r.u32()?;
            let per_chunk = r.u32()?;
            r.u32()?;
            Ok((first, per_chunk))
        })
        .collect::<Result<_, String>>()?;

    let mut offsets = Vec::with_capacity(sizes.len());
    for (i, &(first, per_chunk)) in runs.iter().enumerate() {
        let last = runs
            .get(i + 1)
            .map_or(chunks.len() as u64, |next| u64::from(next.0) - 1);
        if first == 0 || u64::from(first) > last + 1 {
            return Err("bad stsc table".into());
        }
        for chunk in u64::from(first)..=last {
            let mut offset = *chunks.get(chunk as usize - 1).ok_or("bad stsc table")?;
            for _ in 0..per_chunk {
                let Some(&size) = sizes.get(offsets.len()) else {
                    return Ok(offsets);
                };
                offsets.push(offset);
                offset += u64::from(size);
            }
        }
    }
    if offsets.len() < sizes.len() {
        return Err("the chunk tables hold fewer samples than stsz".into());
    }
    Ok(offsets)
}

fn read_times(stts: &[u8], n: usize) -> Result<Vec<u64>, String> {
    let mut r = Reader::new(stts);
    r.full()?;
    let mut times = Vec::with_capacity(n);
    let mut t = 0u64;
    for _ in 0..r.count(8)? {
        let (count, delta) = (r.u32()?, r.u32()?);
        for _ in 0..count {
            if times.len() == n {
                break;
            }
            times.push(t);
            t = t.saturating_add(u64::from(delta));
        }
    }
    if times.len() < n {
        return Err("stts covers fewer samples than stsz".into());
    }
    Ok(times)
}

fn read_composition(ctts: &[u8], n: usize) -> Result<Vec<i64>, String> {
    let mut r = Reader::new(ctts);
    let (version, _) = r.full()?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..r.count(8)? {
        let count = r.u32()?;
        let raw = r.u32()?;
        let offset = if version == 1 {
            i64::from(raw as i32)
        } else {
            i64::from(raw)
        };
        for _ in 0..count {
            if out.len() == n {
                break;
            }
            out.push(offset);
        }
    }
    out.resize(n, 0);
    Ok(out)
}

/// The `DecoderSpecificInfo` bytes and object type from an `esds` box.
pub fn esds_config(esds: &[u8]) -> Result<(u8, Vec<u8>), String> {
    let mut r = Reader::new(esds);
    r.full()?;
    let mut object_type = None;
    // Walk the descriptor tree: ES (3) holds DecoderConfig (4) holds DecoderSpecificInfo (5).
    while r.pos < esds.len() {
        let tag = r.u8()?;
        let mut len = 0usize;
        for _ in 0..4 {
            let b = r.u8()?;
            len = (len << 7) | usize::from(b & 0x7F);
            if b & 0x80 == 0 {
                break;
            }
        }
        match tag {
            3 => {
                r.u16()?;
                let flags = r.u8()?;
                if flags & 0x80 != 0 {
                    r.u16()?;
                }
                if flags & 0x40 != 0 {
                    let url_len = r.u8()?;
                    r.bytes(usize::from(url_len))?;
                }
                if flags & 0x20 != 0 {
                    r.u16()?;
                }
            }
            4 => {
                object_type = Some(r.u8()?);
                r.bytes(12)?;
            }
            5 => {
                let config = r.bytes(len)?.to_vec();
                return Ok((object_type.ok_or("esds has no decoder config")?, config));
            }
            _ => {
                r.bytes(len)?;
            }
        }
    }
    Err("esds has no decoder specific info".into())
}

fn put_box(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
}

fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 8);
    put_box(&mut out, kind, body);
    out
}

fn full_box(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut data = ((u32::from(version) << 24) | flags).to_be_bytes().to_vec();
    data.extend_from_slice(body);
    boxed(kind, &data)
}

fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

const MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

fn matrix() -> Vec<u8> {
    MATRIX.iter().flat_map(|v| v.to_be_bytes()).collect()
}

/// An AAC-LC track to write.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioTrack<'a> {
    pub sample_rate: u32,
    pub channels: u16,
    /// The `AudioSpecificConfig`.
    pub config: &'a [u8],
    /// Raw access units, 1024 samples each.
    pub packets: &'a [Vec<u8>],
    /// Encoder delay at the start, in samples, hidden by the edit list.
    pub priming: u32,
    /// Samples to present after the priming.
    pub length: u64,
}

/// An H.264 track to write: progressive frames at a constant rate, each shown
/// in the order it is stored.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoTrack<'a> {
    pub width: u16,
    pub height: u16,
    /// Each frame lasts `delta / timescale` seconds.
    pub timescale: u32,
    pub delta: u32,
    /// The sequence and picture parameter sets, each a NAL unit with its header.
    pub sps: &'a [u8],
    pub pps: &'a [u8],
    /// One frame per sample, NAL units with 4-byte length prefixes.
    pub samples: &'a [Vec<u8>],
    /// Which samples are key frames.
    pub sync: &'a [bool],
    /// How long to present, in milliseconds. The edit list cuts the last frame short.
    pub length_ms: u64,
}

/// One track as the writer lays it out.
struct Out<'a> {
    handler: &'a [u8; 4],
    name: &'a [u8],
    media_header: Vec<u8>,
    entry: Vec<u8>,
    timescale: u32,
    /// Every sample lasts this long, in `timescale` units.
    delta: u32,
    samples: &'a [Vec<u8>],
    /// Key frames, 1-based; `None` when every sample is one.
    sync: Option<Vec<u32>>,
    /// The single edit: its length in movie units and where in the media it starts.
    edit_duration: u64,
    media_time: u64,
    volume: u16,
    width: u16,
    height: u16,
    /// Samples per chunk, about a second's worth.
    chunk: usize,
}

impl Out<'_> {
    fn chunks(&self) -> usize {
        self.samples.len().div_ceil(self.chunk)
    }
}

fn audio_out<'a>(track: &AudioTrack<'a>, edit_duration: u64) -> Out<'a> {
    let rate = track.sample_rate;
    let data_len: usize = track.packets.iter().map(Vec::len).sum();
    let seconds = (track.length as f64 / f64::from(rate)).max(1e-9);
    let avg_bitrate = (data_len as f64 * 8.0 / seconds) as u32;
    let max_packet = track.packets.iter().map(Vec::len).max().unwrap_or(0) as u32;
    let peak_bitrate =
        u32::try_from(u64::from(max_packet) * 8 * u64::from(rate) / 1024).unwrap_or(u32::MAX);
    let descriptor = |tag: u8, body: &[u8]| cat(&[&[tag, body.len() as u8], body]);
    let decoder_config = descriptor(
        4,
        &cat(&[
            &[0x40, 0x15],
            &max_packet.to_be_bytes()[1..],
            &avg_bitrate.max(peak_bitrate).to_be_bytes(),
            &avg_bitrate.to_be_bytes(),
            &descriptor(5, track.config),
        ]),
    );
    let es = descriptor(
        3,
        &cat(&[
            &1u16.to_be_bytes(),
            &[0],
            &decoder_config,
            &descriptor(6, &[2]),
        ]),
    );
    let mp4a = boxed(
        b"mp4a",
        &cat(&[
            &[0; 6],
            &1u16.to_be_bytes(),
            &[0; 8],
            &track.channels.to_be_bytes(),
            &16u16.to_be_bytes(),
            &[0; 4],
            &(if rate <= 0xFFFF { rate << 16 } else { 0 }).to_be_bytes(),
            &full_box(b"esds", 0, 0, &es),
        ]),
    );
    Out {
        handler: b"soun",
        name: b"Sound\0",
        media_header: full_box(b"smhd", 0, 0, &[0; 4]),
        entry: mp4a,
        timescale: rate,
        delta: 1024,
        samples: track.packets,
        sync: None,
        edit_duration,
        media_time: u64::from(track.priming),
        volume: 0x0100,
        width: 0,
        height: 0,
        chunk: (rate as usize / 1024).max(1),
    }
}

fn video_out<'a>(track: &VideoTrack<'a>) -> Out<'a> {
    let byte = |i: usize| track.sps.get(i).copied().unwrap_or(0);
    let avcc = boxed(
        b"avcC",
        &cat(&[
            &[1, byte(1), byte(2), byte(3), 0xFF, 0xE1],
            &(track.sps.len() as u16).to_be_bytes(),
            track.sps,
            &[1],
            &(track.pps.len() as u16).to_be_bytes(),
            track.pps,
        ]),
    );
    // BT.709 primaries, transfer and matrix, limited range (SPEC.md section 5.7).
    let colr = boxed(
        b"colr",
        &cat(&[
            b"nclx",
            &1u16.to_be_bytes(),
            &1u16.to_be_bytes(),
            &1u16.to_be_bytes(),
            &[0],
        ]),
    );
    let avc1 = boxed(
        b"avc1",
        &cat(&[
            &[0; 6],
            &1u16.to_be_bytes(),
            &[0; 16],
            &track.width.to_be_bytes(),
            &track.height.to_be_bytes(),
            &0x0048_0000u32.to_be_bytes(),
            &0x0048_0000u32.to_be_bytes(),
            &[0; 4],
            &1u16.to_be_bytes(),
            &[0; 32],
            &0x0018u16.to_be_bytes(),
            &0xFFFFu16.to_be_bytes(),
            &avcc,
            &colr,
        ]),
    );
    let sync = track
        .sync
        .iter()
        .enumerate()
        .filter(|(_, s)| **s)
        .map(|(i, _)| i as u32 + 1)
        .collect();
    Out {
        handler: b"vide",
        name: b"Video\0",
        media_header: full_box(b"vmhd", 0, 1, &[0; 8]),
        entry: avc1,
        timescale: track.timescale,
        delta: track.delta,
        samples: track.samples,
        sync: Some(sync),
        edit_duration: track.length_ms,
        media_time: 0,
        volume: 0,
        width: track.width,
        height: track.height,
        chunk: (track.timescale / track.delta.max(1)).max(1) as usize,
    }
}

/// Writes an M4A file holding one AAC-LC track, with the index before the data
/// so it can play while downloading.
pub fn write_m4a(track: &AudioTrack) -> Vec<u8> {
    write(
        b"M4A ",
        b"M4A mp42isom",
        track.sample_rate,
        &[audio_out(track, track.length)],
    )
}

/// Writes an MP4 file holding an H.264 track and, if given, an AAC-LC track,
/// index first. Both are presented for `video.length_ms`.
pub fn write_mp4(video: &VideoTrack, audio: Option<&AudioTrack>) -> Vec<u8> {
    let mut tracks = vec![video_out(video)];
    if let Some(audio) = audio {
        tracks.push(audio_out(audio, video.length_ms));
    }
    write(b"isom", b"isomiso2avc1mp41", 1000, &tracks)
}

fn write(brand: &[u8; 4], compatible: &[u8], movie_timescale: u32, tracks: &[Out]) -> Vec<u8> {
    let ftyp = boxed(b"ftyp", &cat(&[brand, &0u32.to_be_bytes(), compatible]));
    let most_chunks = tracks.iter().map(Out::chunks).max().unwrap_or(0);
    // Chunks interleave: chunk i of each track in turn. Offsets are from the
    // start of the media data.
    let mut offsets: Vec<Vec<u64>> = vec![Vec::new(); tracks.len()];
    let mut data_len = 0u64;
    for i in 0..most_chunks {
        for (t, track) in tracks.iter().enumerate() {
            if i < track.chunks() {
                offsets[t].push(data_len);
                let end = ((i + 1) * track.chunk).min(track.samples.len());
                data_len += track.samples[i * track.chunk..end]
                    .iter()
                    .map(|s| s.len() as u64)
                    .sum::<u64>();
            }
        }
    }

    let moov_for = |data_offset: u64| {
        let wide = data_offset + data_len > u64::from(u32::MAX);
        let movie_duration = tracks.iter().map(|t| t.edit_duration).max().unwrap_or(0);
        let mvhd = full_box(
            b"mvhd",
            1,
            0,
            &cat(&[
                &[0; 16],
                &movie_timescale.to_be_bytes(),
                &movie_duration.to_be_bytes(),
                &0x0001_0000u32.to_be_bytes(),
                &0x0100u16.to_be_bytes(),
                &[0; 10],
                &matrix(),
                &[0; 24],
                &(tracks.len() as u32 + 1).to_be_bytes(),
            ]),
        );
        let mut moov = mvhd;
        for (t, track) in tracks.iter().enumerate() {
            moov.extend_from_slice(&trak(track, t as u32 + 1, &offsets[t], data_offset, wide));
        }
        boxed(b"moov", &moov)
    };

    let mdat_header = if data_len + 8 > u64::from(u32::MAX) {
        16
    } else {
        8
    };
    // The index size depends only on whether offsets need 64 bits.
    let guess = moov_for(0).len() as u64;
    let mut data_offset = ftyp.len() as u64 + guess + mdat_header;
    let mut moov = moov_for(data_offset);
    if moov.len() as u64 != guess {
        data_offset = ftyp.len() as u64 + moov.len() as u64 + mdat_header;
        moov = moov_for(data_offset);
    }

    let mut out = Vec::with_capacity((data_offset + data_len) as usize);
    out.extend_from_slice(&ftyp);
    out.extend_from_slice(&moov);
    if mdat_header == 16 {
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(b"mdat");
        out.extend_from_slice(&(data_len + 16).to_be_bytes());
    } else {
        out.extend_from_slice(&((data_len + 8) as u32).to_be_bytes());
        out.extend_from_slice(b"mdat");
    }
    for i in 0..most_chunks {
        for track in tracks {
            if i < track.chunks() {
                let end = ((i + 1) * track.chunk).min(track.samples.len());
                for sample in &track.samples[i * track.chunk..end] {
                    out.extend_from_slice(sample);
                }
            }
        }
    }
    out
}

fn trak(track: &Out, id: u32, offsets: &[u64], data_offset: u64, wide: bool) -> Vec<u8> {
    let n = track.samples.len();
    let tkhd = full_box(
        b"tkhd",
        1,
        3,
        &cat(&[
            &[0; 16],
            &id.to_be_bytes(),
            &[0; 4],
            &track.edit_duration.to_be_bytes(),
            &[0; 12],
            &track.volume.to_be_bytes(),
            &[0; 2],
            &matrix(),
            &(u32::from(track.width) << 16).to_be_bytes(),
            &(u32::from(track.height) << 16).to_be_bytes(),
        ]),
    );
    let elst = full_box(
        b"elst",
        1,
        0,
        &cat(&[
            &1u32.to_be_bytes(),
            &track.edit_duration.to_be_bytes(),
            &track.media_time.to_be_bytes(),
            &1u16.to_be_bytes(),
            &0u16.to_be_bytes(),
        ]),
    );
    let mdhd = full_box(
        b"mdhd",
        1,
        0,
        &cat(&[
            &[0; 16],
            &track.timescale.to_be_bytes(),
            &(n as u64 * u64::from(track.delta)).to_be_bytes(),
            &0x55C4u16.to_be_bytes(),
            &[0; 2],
        ]),
    );
    let hdlr = full_box(
        b"hdlr",
        0,
        0,
        &cat(&[&[0; 4], track.handler, &[0; 12], track.name]),
    );
    let dinf = boxed(
        b"dinf",
        &full_box(
            b"dref",
            0,
            0,
            &cat(&[&1u32.to_be_bytes(), &full_box(b"url ", 0, 1, &[])]),
        ),
    );
    let stsd = full_box(b"stsd", 0, 0, &cat(&[&1u32.to_be_bytes(), &track.entry]));
    let stts = if n == 0 {
        full_box(b"stts", 0, 0, &0u32.to_be_bytes())
    } else {
        full_box(
            b"stts",
            0,
            0,
            &cat(&[
                &1u32.to_be_bytes(),
                &(n as u32).to_be_bytes(),
                &track.delta.to_be_bytes(),
            ]),
        )
    };
    // Every chunk is full except perhaps the last.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for i in 0..track.chunks() {
        let count = ((i + 1) * track.chunk).min(n) - i * track.chunk;
        if runs.last().is_none_or(|(_, c)| *c != count) {
            runs.push((i + 1, count));
        }
    }
    let mut stsc = (runs.len() as u32).to_be_bytes().to_vec();
    for (first, count) in runs {
        stsc.extend_from_slice(&(first as u32).to_be_bytes());
        stsc.extend_from_slice(&(count as u32).to_be_bytes());
        stsc.extend_from_slice(&1u32.to_be_bytes());
    }
    let stsc = full_box(b"stsc", 0, 0, &stsc);
    let mut sizes = 0u32.to_be_bytes().to_vec();
    sizes.extend_from_slice(&(n as u32).to_be_bytes());
    for s in track.samples {
        sizes.extend_from_slice(&(s.len() as u32).to_be_bytes());
    }
    let stsz = full_box(b"stsz", 0, 0, &sizes);
    let mut table = (offsets.len() as u32).to_be_bytes().to_vec();
    for offset in offsets {
        let at = data_offset + offset;
        if wide {
            table.extend_from_slice(&at.to_be_bytes());
        } else {
            table.extend_from_slice(&(at as u32).to_be_bytes());
        }
    }
    let chunk_offsets = full_box(if wide { b"co64" } else { b"stco" }, 0, 0, &table);
    let mut stbl = cat(&[&stsd, &stts, &stsc, &stsz, &chunk_offsets]);
    if let Some(sync) = &track.sync {
        let mut body = (sync.len() as u32).to_be_bytes().to_vec();
        for s in sync {
            body.extend_from_slice(&s.to_be_bytes());
        }
        stbl.extend_from_slice(&full_box(b"stss", 0, 0, &body));
    }
    let minf = boxed(
        b"minf",
        &cat(&[&track.media_header, &dinf, &boxed(b"stbl", &stbl)]),
    );
    let mdia = boxed(b"mdia", &cat(&[&mdhd, &hdlr, &minf]));
    boxed(b"trak", &cat(&[&tkhd, &boxed(b"edts", &elst), &mdia]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written() -> Vec<u8> {
        let packets = vec![vec![1, 2, 3], vec![4, 5], vec![6]];
        write_m4a(&AudioTrack {
            sample_rate: 48000,
            channels: 2,
            config: &[0x11, 0x90],
            packets: &packets,
            priming: 1024,
            length: 2000,
        })
    }

    #[test]
    fn written_files_read_back() {
        let file = written();
        assert_eq!(&file[4..8], b"ftyp");
        let movie = read(&file, 100).unwrap();
        assert_eq!(movie.timescale, 48000);
        let track = &movie.tracks[0];
        assert_eq!((&track.handler, track.timescale), (b"soun", 48000));
        assert_eq!(&track.entry.format, b"mp4a");
        assert_eq!(
            track.edits,
            Some(vec![Edit {
                duration: 2000,
                media_time: 1024
            }])
        );
        let data: Vec<&[u8]> = track
            .samples
            .iter()
            .map(|s| track.sample_data(&file, s).unwrap())
            .collect();
        assert_eq!(data, [&[1, 2, 3][..], &[4, 5], &[6]]);
        assert_eq!(
            track
                .samples
                .iter()
                .map(|s| s.decode_time)
                .collect::<Vec<_>>(),
            [0, 1024, 2048]
        );
        assert!(track.samples.iter().all(|s| s.sync));

        let children = track.entry_children(28).unwrap();
        let (object_type, config) = esds_config(child(&children, b"esds").unwrap()).unwrap();
        assert_eq!((object_type, config), (0x40, vec![0x11, 0x90]));

        // The core crate accepts it as a carrier.
        assert_eq!(
            unbaked_core::detect(&file),
            Some(unbaked_core::Container::Mp4)
        );
    }

    #[test]
    fn video_files_interleave_and_read_back() {
        // 25 frames at 10 fps with a key frame every 10, and 2.5 s of 8 kHz sound.
        let frames: Vec<Vec<u8>> = (0..25u8).map(|i| vec![0, 0, 0, 1, i]).collect();
        let sync: Vec<bool> = (0..25).map(|i| i % 10 == 0).collect();
        let packets: Vec<Vec<u8>> = (0..20u8).map(|i| vec![0xA0, i]).collect();
        let audio = AudioTrack {
            sample_rate: 8000,
            channels: 1,
            config: &[0x15, 0x88],
            packets: &packets,
            priming: 1024,
            length: 20000,
        };
        let video = VideoTrack {
            width: 96,
            height: 64,
            timescale: 90000,
            delta: 9000,
            sps: &[0x67, 0x42, 0xC0, 0x0A, 0xAB],
            pps: &[0x68, 0xCE],
            samples: &frames,
            sync: &sync,
            length_ms: 2450,
        };
        let file = write_mp4(&video, Some(&audio));
        assert_eq!(
            unbaked_core::detect(&file),
            Some(unbaked_core::Container::Mp4)
        );
        let movie = read(&file, 100).unwrap();
        assert_eq!(movie.timescale, 1000);
        let [v, a] = &movie.tracks[..] else {
            panic!("two tracks")
        };
        assert_eq!((&v.handler, v.timescale, v.rotation), (b"vide", 90000, 0));
        assert_eq!(
            v.edits,
            Some(vec![Edit {
                duration: 2450,
                media_time: 0
            }])
        );
        assert_eq!(
            a.edits,
            Some(vec![Edit {
                duration: 2450,
                media_time: 1024
            }])
        );
        for (i, s) in v.samples.iter().enumerate() {
            assert_eq!(v.sample_data(&file, s).unwrap(), &frames[i][..]);
            assert_eq!(
                (s.decode_time, s.composition_offset, s.sync),
                (i as u64 * 9000, 0, i % 10 == 0)
            );
        }
        for (i, s) in a.samples.iter().enumerate() {
            assert_eq!(a.sample_data(&file, s).unwrap(), &packets[i][..]);
        }
        let children = v.entry_children(78).unwrap();
        assert_eq!(
            child(&children, b"avcC").unwrap(),
            [
                1, 0x42, 0xC0, 0x0A, 0xFF, 0xE1, 0, 5, 0x67, 0x42, 0xC0, 0x0A, 0xAB, 1, 0, 2, 0x68,
                0xCE
            ]
        );
        // Chunks alternate: the first second of video, then of sound.
        let first_audio = a.samples[0].offset;
        assert_eq!(first_audio, v.samples[9].offset + 5);
        assert_eq!(v.samples[10].offset, a.samples[6].offset + 2);
    }

    #[test]
    fn hostile_tables_are_errors_not_panics() {
        let file = written();
        assert!(read(&file, 2).unwrap_err().contains("more than the limit"));
        // Corrupt every byte of the index in turn: reading must never panic.
        let ftyp = u32::from_be_bytes(file[..4].try_into().unwrap()) as usize;
        let moov_end = ftyp + u32::from_be_bytes(file[ftyp..ftyp + 4].try_into().unwrap()) as usize;
        for i in 0..moov_end.min(file.len()) {
            for v in [0u8, 0xFF, 0x7F] {
                let mut bad = file.clone();
                bad[i] = v;
                if let Ok(movie) = read(&bad, 1000) {
                    for t in &movie.tracks {
                        for s in &t.samples {
                            let _ = t.sample_data(&bad, s);
                        }
                    }
                }
            }
        }
        assert!(read(b"\0\0\0\x08moof", 10).is_err());
        assert!(read(&[0, 0, 0, 200, b'm', b'o', b'o', b'v'], 10).is_err());
    }
}
