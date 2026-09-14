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
        timescale,
        entry,
        samples,
        edits,
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

/// Writes an M4A file holding one AAC-LC track, with the index before the data
/// so it can play while downloading.
pub fn write_m4a(track: &AudioTrack) -> Vec<u8> {
    let rate = track.sample_rate;
    let media_len = track.packets.len() as u64 * 1024;
    let data_len: usize = track.packets.iter().map(Vec::len).sum();
    let seconds = (track.length as f64 / f64::from(rate)).max(1e-9);
    let avg_bitrate = (data_len as f64 * 8.0 / seconds) as u32;
    let max_packet = track.packets.iter().map(Vec::len).max().unwrap_or(0) as u32;
    let peak_bitrate =
        u32::try_from(u64::from(max_packet) * 8 * u64::from(rate) / 1024).unwrap_or(u32::MAX);

    let ftyp = boxed(
        b"ftyp",
        &cat(&[b"M4A ", &0u32.to_be_bytes(), b"M4A mp42isom"]),
    );
    let moov_for = |data_offset: u64| {
        let wide = data_offset + data_len as u64 > u64::from(u32::MAX);
        let mvhd = full_box(
            b"mvhd",
            1,
            0,
            &cat(&[
                &[0; 16],
                &rate.to_be_bytes(),
                &track.length.to_be_bytes(),
                &0x0001_0000u32.to_be_bytes(),
                &0x0100u16.to_be_bytes(),
                &[0; 10],
                &matrix(),
                &[0; 24],
                &2u32.to_be_bytes(),
            ]),
        );
        let tkhd = full_box(
            b"tkhd",
            1,
            3,
            &cat(&[
                &[0; 16],
                &1u32.to_be_bytes(),
                &[0; 4],
                &track.length.to_be_bytes(),
                &[0; 12],
                &0x0100u16.to_be_bytes(),
                &[0; 2],
                &matrix(),
                &[0; 8],
            ]),
        );
        let elst = full_box(
            b"elst",
            1,
            0,
            &cat(&[
                &1u32.to_be_bytes(),
                &track.length.to_be_bytes(),
                &u64::from(track.priming).to_be_bytes(),
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
                &rate.to_be_bytes(),
                &media_len.to_be_bytes(),
                &0x55C4u16.to_be_bytes(),
                &[0; 2],
            ]),
        );
        let hdlr = full_box(
            b"hdlr",
            0,
            0,
            &cat(&[&[0; 4], b"soun", &[0; 12], b"Sound\0"]),
        );
        let smhd = full_box(b"smhd", 0, 0, &[0; 4]);
        let dinf = boxed(
            b"dinf",
            &full_box(
                b"dref",
                0,
                0,
                &cat(&[&1u32.to_be_bytes(), &full_box(b"url ", 0, 1, &[])]),
            ),
        );

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
        let esds = full_box(b"esds", 0, 0, &es);
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
                &esds,
            ]),
        );
        let stsd = full_box(b"stsd", 0, 0, &cat(&[&1u32.to_be_bytes(), &mp4a]));
        let stts = full_box(
            b"stts",
            0,
            0,
            &cat(&[
                &1u32.to_be_bytes(),
                &(track.packets.len() as u32).to_be_bytes(),
                &1024u32.to_be_bytes(),
            ]),
        );
        let stsc = full_box(
            b"stsc",
            0,
            0,
            &cat(&[
                &1u32.to_be_bytes(),
                &1u32.to_be_bytes(),
                &(track.packets.len() as u32).to_be_bytes(),
                &1u32.to_be_bytes(),
            ]),
        );
        let mut sizes = 0u32.to_be_bytes().to_vec();
        sizes.extend_from_slice(&(track.packets.len() as u32).to_be_bytes());
        for p in track.packets {
            sizes.extend_from_slice(&(p.len() as u32).to_be_bytes());
        }
        let stsz = full_box(b"stsz", 0, 0, &sizes);
        let chunk_offset = if wide {
            full_box(
                b"co64",
                0,
                0,
                &cat(&[&1u32.to_be_bytes(), &data_offset.to_be_bytes()]),
            )
        } else {
            full_box(
                b"stco",
                0,
                0,
                &cat(&[&1u32.to_be_bytes(), &(data_offset as u32).to_be_bytes()]),
            )
        };
        let stbl = boxed(b"stbl", &cat(&[&stsd, &stts, &stsc, &stsz, &chunk_offset]));
        let minf = boxed(b"minf", &cat(&[&smhd, &dinf, &stbl]));
        let mdia = boxed(b"mdia", &cat(&[&mdhd, &hdlr, &minf]));
        let trak = boxed(b"trak", &cat(&[&tkhd, &boxed(b"edts", &elst), &mdia]));
        boxed(b"moov", &cat(&[&mvhd, &trak]))
    };

    let mdat_header = if data_len as u64 + 8 > u64::from(u32::MAX) {
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

    let mut out = Vec::with_capacity(data_offset as usize + data_len);
    out.extend_from_slice(&ftyp);
    out.extend_from_slice(&moov);
    if mdat_header == 16 {
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(b"mdat");
        out.extend_from_slice(&(data_len as u64 + 16).to_be_bytes());
    } else {
        out.extend_from_slice(&((data_len + 8) as u32).to_be_bytes());
        out.extend_from_slice(b"mdat");
    }
    for p in track.packets {
        out.extend_from_slice(p);
    }
    out
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
