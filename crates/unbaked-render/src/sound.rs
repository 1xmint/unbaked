//! Sound (SPEC.md section 6): decoding sources, resampling, mixing clips and
//! encoding AAC-LC into an M4A file.

use std::collections::HashMap;
use std::io::Cursor;

use rusty_aac::{AacEncoder, AacEncoderConfig, audio_specific_config_bytes};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::well_known::CODEC_ID_AAC;
use symphonia::core::codecs::audio::{AudioCodecParameters, AudioDecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::packet::Packet;
use symphonia::core::units::{Duration, Timestamp};
use unbaked_core::recipe::{AssetSource, AudioClip, Recipe};
use unbaked_core::sniff::{self, AssetKind};

use crate::RenderError;
use crate::motion::value_at;
use crate::mp4;
use crate::scene::{Deadline, RenderLimits};

/// Decoded sound: one sample vector per channel, all the same length.
#[derive(Debug, Clone, PartialEq)]
pub struct Pcm {
    pub rate: u32,
    pub channels: Vec<Vec<f32>>,
}

impl Pcm {
    pub fn len(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Encoder delay of the AAC encoder, in samples.
pub const AAC_PRIMING: u32 = 1024;

/// Decodes an audio file, or the first sound track of an MP4 file. `max_samples`
/// caps the samples per channel.
pub fn decode(bytes: &[u8], max_samples: u64) -> Result<Pcm, String> {
    decode_within(bytes, max_samples, None)
}

/// [`decode`], stopping with an error once `deadline` passes. The deadline is
/// checked between decoded chunks.
fn decode_within(
    bytes: &[u8],
    max_samples: u64,
    deadline: Option<Deadline>,
) -> Result<Pcm, String> {
    let head = &bytes[..bytes.len().min(sniff::HEADER_LEN)];
    let pcm = match sniff::detect(head) {
        Some(AssetKind::Mp4) => decode_mp4(bytes, max_samples, deadline)?,
        Some(AssetKind::Mp3 | AssetKind::Wav | AssetKind::Flac) => {
            check_wav_channels(bytes)?;
            decode_other(bytes, max_samples, deadline)?
        }
        _ => return Err("not an audio file".into()),
    };
    if pcm.rate == 0 || pcm.channels.is_empty() {
        return Err("the sound has no sample rate or no channels".into());
    }
    Ok(pcm)
}

/// Rejects WAV data declaring more than 2 channels before symphonia reads it:
/// symphonia-format-riff 0.6.1 overflows a `u16` on huge channel counts, which
/// panics wherever overflow checks are on. Every `RIFF….WAVE` header in the
/// file is checked, since symphonia's probe may find one after other data.
fn check_wav_channels(bytes: &[u8]) -> Result<(), String> {
    let u32_at = |i: usize| {
        bytes
            .get(i..i + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    let starts = bytes
        .windows(12)
        .enumerate()
        .filter(|(_, w)| w.starts_with(b"RIFF") && w.ends_with(b"WAVE"));
    for (start, _) in starts {
        let mut pos = start + 12;
        while let (Some(kind), Some(len)) = (bytes.get(pos..pos + 4), u32_at(pos + 4)) {
            if kind == b"fmt " {
                if let Some(b) = bytes.get(pos + 10..pos + 12) {
                    let n = u16::from_le_bytes([b[0], b[1]]);
                    if n > 2 {
                        return Err(format!(
                            "{n} channels: sources with more than 2 channels are not supported"
                        ));
                    }
                }
                break;
            }
            pos = pos
                .saturating_add(8)
                .saturating_add(len)
                .saturating_add(len % 2);
        }
    }
    Ok(())
}

fn out_of_time(deadline: Option<Deadline>) -> Result<(), String> {
    match deadline {
        Some(d) if d.passed() => Err("the time limit passed".into()),
        _ => Ok(()),
    }
}

fn too_long(n: usize, max: u64) -> Result<(), String> {
    if n as u64 > max {
        return Err(format!("the sound has more than {max} samples per channel"));
    }
    Ok(())
}

/// Appends a decoded buffer to per-channel vectors.
fn append(pcm: &mut Vec<Vec<f32>>, planes: Vec<Vec<f32>>) -> Result<(), String> {
    if pcm.is_empty() {
        pcm.resize(planes.len(), Vec::new());
    }
    if planes.len() != pcm.len() {
        return Err("the channel count changes mid-stream".into());
    }
    for (all, plane) in pcm.iter_mut().zip(planes) {
        all.extend_from_slice(&plane);
    }
    Ok(())
}

/// MP3, WAV and FLAC through symphonia's own readers, with encoder delay and
/// padding removed.
fn decode_other(bytes: &[u8], max_samples: u64, deadline: Option<Deadline>) -> Result<Pcm, String> {
    let source = MediaSourceStream::new(
        Box::new(Cursor::new(bytes)),
        MediaSourceStreamOptions::default(),
    );
    let mut reader = symphonia::default::get_probe()
        .probe(
            &Hint::new(),
            source,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| e.to_string())?;
    let track = reader
        .first_track(TrackType::Audio)
        .ok_or("no sound track")?;
    let track_id = track.id;
    let Some(CodecParameters::Audio(params)) = &track.codec_params else {
        return Err("no sound track".into());
    };
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| e.to_string())?;
    let mut channels = Vec::new();
    let mut rate = params.sample_rate.unwrap_or(0);
    loop {
        out_of_time(deadline)?;
        let packet = match reader.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(e) => return Err(e.to_string()),
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buffer) => {
                rate = buffer.spec().rate();
                let mut planes = Vec::new();
                buffer.copy_to_vecs_planar::<f32>(&mut planes);
                append(&mut channels, planes)?;
                too_long(channels[0].len(), max_samples)?;
            }
            // A damaged frame is skipped, as players do.
            Err(SymphoniaError::DecodeError(_)) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(Pcm { rate, channels })
}

/// AAC-LC from an MP4 file's first sound track, placed on the track's
/// timeline by its edit list.
fn decode_mp4(bytes: &[u8], max_samples: u64, deadline: Option<Deadline>) -> Result<Pcm, String> {
    let movie = mp4::read(bytes, max_samples)?;
    let track = movie
        .tracks
        .iter()
        .find(|t| &t.handler == b"soun")
        .ok_or("the file has no sound track")?;
    if &track.entry.format != b"mp4a" {
        return Err(format!(
            "sound format {} is not supported",
            String::from_utf8_lossy(&track.entry.format)
        ));
    }
    // QuickTime sound descriptions add 16 (version 1) or 36 (version 2) bytes.
    let version = u16::from_be_bytes(
        track
            .entry
            .body
            .get(8..10)
            .ok_or("bad mp4a box")?
            .try_into()
            .unwrap(),
    );
    let fixed = match version {
        0 => 28,
        1 => 44,
        2 => 64,
        _ => return Err("bad mp4a box".into()),
    };
    let children = track.entry_children(fixed)?;
    let esds = children
        .iter()
        .find(|(k, _)| k == b"esds")
        .map(|(_, d)| *d)
        .or_else(|| {
            let wave = children.iter().find(|(k, _)| k == b"wave")?.1;
            mp4::boxes(wave)
                .ok()?
                .into_iter()
                .find(|(k, _)| k == b"esds")
                .map(|(_, d)| d)
        })
        .ok_or("the mp4a box has no esds")?;
    let (object_type, config) = mp4::esds_config(esds)?;
    if object_type != 0x40 && object_type != 0x67 {
        return Err("only AAC sound is supported in MP4 files".into());
    }
    let audio_object = config.first().map(|b| b >> 3).ok_or("empty AAC config")?;
    if audio_object != 2 {
        return Err(format!(
            "AAC object type {audio_object} is not supported, only AAC-LC (2)"
        ));
    }

    let mut params = AudioCodecParameters::new();
    params
        .for_codec(CODEC_ID_AAC)
        .with_extra_data(config.into_boxed_slice());
    let options = AudioDecoderOptions::default().gapless(false);
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &options)
        .map_err(|e| e.to_string())?;
    let mut channels: Vec<Vec<f32>> = Vec::new();
    let mut rate = 0;
    for sample in &track.samples {
        out_of_time(deadline)?;
        let data = track.sample_data(bytes, sample)?;
        let packet = Packet::new(
            0,
            Timestamp::new(sample.decode_time as i64),
            Duration::new(1024),
            data,
        );
        let buffer = decoder.decode(&packet).map_err(|e| e.to_string())?;
        rate = buffer.spec().rate();
        let mut planes = Vec::new();
        buffer.copy_to_vecs_planar::<f32>(&mut planes);
        append(&mut channels, planes)?;
        too_long(channels[0].len(), max_samples)?;
    }
    if rate == 0 {
        return Ok(Pcm { rate, channels });
    }

    let Some(edits) = &track.edits else {
        return Ok(Pcm { rate, channels });
    };
    // Media time in samples: the track timescale is usually the sample rate.
    let to_samples =
        |t: u64, scale: u32| (u128::from(t) * u128::from(rate) / u128::from(scale)) as u64;
    let mut out: Vec<Vec<f32>> = vec![Vec::new(); channels.len()];
    for edit in edits {
        let length = if edit.duration == 0 && edits.len() == 1 {
            None
        } else {
            Some(to_samples(edit.duration, movie.timescale))
        };
        for (all, source) in out.iter_mut().zip(&channels) {
            if edit.media_time < 0 {
                let n = length.unwrap_or(0);
                too_long(all.len().saturating_add(n as usize), max_samples)?;
                all.resize(all.len() + n as usize, 0.0);
                continue;
            }
            let start = to_samples(edit.media_time as u64, track.timescale) as usize;
            let end = match length {
                Some(n) => start.saturating_add(n as usize),
                None => source.len().max(start),
            };
            too_long(all.len().saturating_add(end - start), max_samples)?;
            for i in start..end {
                all.push(source.get(i).copied().unwrap_or(0.0));
            }
        }
    }
    Ok(Pcm {
        rate,
        channels: out,
    })
}

/// A sound source's length, read from its headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Length {
    pub rate: u32,
    pub channels: u32,
    /// Samples per channel. Where the header does not say, a guess from the
    /// file size at 32 kbit/s, which overstates most files.
    pub frames: u64,
}

/// About how many kernel taps resampling reads per sample of the longer side.
pub const RESAMPLE_TAPS: u64 = 34;

/// The length of an audio file or of an MP4's first sound track, without decoding it.
pub fn length(bytes: &[u8], max_samples: u64) -> Result<Length, String> {
    let head = &bytes[..bytes.len().min(sniff::HEADER_LEN)];
    match sniff::detect(head) {
        Some(AssetKind::Mp4) => {
            let movie = mp4::read(bytes, max_samples)?;
            let track = movie
                .tracks
                .iter()
                .find(|t| &t.handler == b"soun")
                .ok_or("the file has no sound track")?;
            // An audio sample entry: 16 bytes, then the channel count.
            let channels = track
                .entry
                .body
                .get(16..18)
                .map_or(2, |b| u32::from(u16::from_be_bytes([b[0], b[1]])));
            Ok(Length {
                rate: track.timescale,
                channels: channels.clamp(1, 2),
                frames: track.samples.len() as u64 * 1024,
            })
        }
        Some(AssetKind::Mp3 | AssetKind::Wav | AssetKind::Flac) => {
            check_wav_channels(bytes)?;
            let source = MediaSourceStream::new(
                Box::new(Cursor::new(bytes)),
                MediaSourceStreamOptions::default(),
            );
            let reader = symphonia::default::get_probe()
                .probe(
                    &Hint::new(),
                    source,
                    FormatOptions::default(),
                    MetadataOptions::default(),
                )
                .map_err(|e| e.to_string())?;
            let track = reader
                .first_track(TrackType::Audio)
                .ok_or("no sound track")?;
            let Some(CodecParameters::Audio(params)) = &track.codec_params else {
                return Err("no sound track".into());
            };
            let rate = params.sample_rate.unwrap_or(48_000);
            let channels = params.channels.as_ref().map_or(2, |c| c.count() as u32);
            let guess = bytes.len() as u64 * 8 * u64::from(rate) / 32_000;
            Ok(Length {
                rate,
                channels: channels.clamp(1, 2),
                frames: track.num_frames.unwrap_or(guess),
            })
        }
        _ => Err("not an audio file".into()),
    }
}

/// Zero crossings of the resampling kernel on each side.
const SINC_ZEROS: f64 = 16.0;

/// Windowed-sinc resampling (Blackman window). The pass band ends at 95% of
/// the lower Nyquist frequency.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    resample_within(input, from, to, None).expect("no deadline to pass")
}

/// [`resample`], or `None` once `deadline` passes.
fn resample_within(
    input: &[f32],
    from: u32,
    to: u32,
    deadline: Option<Deadline>,
) -> Option<Vec<f32>> {
    if from == to || input.is_empty() {
        return Some(input.to_vec());
    }
    let cutoff = 0.95 * f64::from(from.min(to)) / f64::from(from);
    let half = (SINC_ZEROS / cutoff).ceil() as i64;
    let out_len = (input.len() as u64 * u64::from(to)).div_ceil(u64::from(from)) as usize;
    let g = gcd(from, to);
    let (step, phases) = (u64::from(from / g), u64::from(to / g));
    let kernel = |frac: f64| -> Vec<f32> {
        let mut taps: Vec<f64> = (-half + 1..=half)
            .map(|i| {
                let x = i as f64 - frac;
                let sinc = if x == 0.0 {
                    1.0
                } else {
                    let a = std::f64::consts::PI * cutoff * x;
                    a.sin() / a
                };
                let w = (x / half as f64 + 1.0) / 2.0;
                let window = if (0.0..=1.0).contains(&w) {
                    let t = 2.0 * std::f64::consts::PI * w;
                    0.42 - 0.5 * t.cos() + 0.08 * (2.0 * t).cos()
                } else {
                    0.0
                };
                sinc * window
            })
            .collect();
        let sum: f64 = taps.iter().sum();
        taps.iter_mut().for_each(|t| *t /= sum);
        taps.into_iter().map(|t| t as f32).collect()
    };
    let table: Option<Vec<Vec<f32>>> = (phases <= 4096).then(|| {
        (0..phases)
            .map(|p| kernel(p as f64 / phases as f64))
            .collect()
    });

    let mut out = Vec::with_capacity(out_len);
    for k in 0..out_len as u64 {
        if k % 65_536 == 0 && deadline.is_some_and(|d| d.passed()) {
            return None;
        }
        // Output sample k sits at source position k·from/to.
        let pos = k * step;
        let (whole, phase) = ((pos / phases) as i64, pos % phases);
        let owned;
        let taps = match &table {
            Some(t) => &t[phase as usize],
            None => {
                owned = kernel(phase as f64 / phases as f64);
                &owned
            }
        };
        let mut acc = 0.0f32;
        for (t, i) in taps.iter().zip(whole - half + 1..) {
            if let Some(&s) = usize::try_from(i).ok().and_then(|i| input.get(i)) {
                acc += s * t;
            }
        }
        out.push(acc);
    }
    Some(out)
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Mono to stereo copies the channel; stereo to mono averages the two.
fn map_channels(mut channels: Vec<Vec<f32>>, wanted: u8) -> Result<Vec<Vec<f32>>, String> {
    match (channels.len(), wanted) {
        (n, w) if n == usize::from(w) => Ok(channels),
        (1, 2) => {
            let only = channels.remove(0);
            Ok(vec![only.clone(), only])
        }
        (2, 1) => Ok(vec![
            channels[0]
                .iter()
                .zip(&channels[1])
                .map(|(l, r)| (l + r) / 2.0)
                .collect(),
        ]),
        (n, _) => Err(format!(
            "{n} channels: sources with more than 2 channels are not supported"
        )),
    }
}

/// Mixes the recipe's audio clips over `output.duration_ms`. `file` returns a
/// package file's bytes by path.
pub fn mix<'a>(
    recipe: &'a Recipe,
    file: &'a dyn Fn(&str) -> Option<&'a [u8]>,
    limits: RenderLimits,
) -> Result<Pcm, RenderError> {
    let max_samples = limits.max_samples;
    let output = &recipe.output;
    let rate = output.sample_rate;
    let duration_ms = output.duration_ms.unwrap_or(0);
    let total = (duration_ms * u64::from(rate)).div_ceil(1000);
    if total > max_samples {
        return Err(RenderError::TooLong {
            what: "the sound".into(),
            samples: total,
            limit: max_samples,
        });
    }
    let mut channels = vec![vec![0.0f32; total as usize]; usize::from(output.channels)];
    let mut sources: HashMap<&str, Pcm> = HashMap::new();

    for (i, clip) in recipe.audio.iter().enumerate() {
        if clip.muted {
            continue;
        }
        limits.check_time()?;
        let path = format!("/audio/{i}");
        if !sources.contains_key(clip.asset.as_str()) {
            let asset = recipe
                .assets
                .get(&clip.asset)
                .ok_or_else(|| RenderError::Unsupported(format!("{path}: asset is missing")))?;
            let AssetSource::Path(file_path) = &asset.source else {
                return Err(RenderError::Unsupported(format!(
                    "{path}: asset is not a file"
                )));
            };
            let bytes = file(file_path).ok_or_else(|| {
                RenderError::Unsupported(format!("{path}: {file_path} is missing"))
            })?;
            let fail = |message: String| RenderError::Decode {
                asset: file_path.clone(),
                message,
            };
            let decoded = match decode_within(bytes, max_samples, limits.deadline) {
                Ok(decoded) => decoded,
                Err(message) => {
                    limits.check_time()?;
                    return Err(fail(message));
                }
            };
            let resampled = decoded
                .channels
                .iter()
                .map(|c| resample_within(c, decoded.rate, rate, limits.deadline))
                .collect::<Option<Vec<Vec<f32>>>>();
            // Resampling stops early only when the deadline has passed.
            let Some(resampled) = resampled else {
                return Err(RenderError::TimedOut {
                    limit_ms: limits.deadline.map_or(0, |d| d.limit_ms()),
                });
            };
            let mapped = map_channels(resampled, output.channels).map_err(fail)?;
            sources.insert(
                &clip.asset,
                Pcm {
                    rate,
                    channels: mapped,
                },
            );
        }
        add_clip(&mut channels, clip, &sources[clip.asset.as_str()]);
    }
    for c in &mut channels {
        for s in c.iter_mut() {
            *s = s.clamp(-1.0, 1.0);
        }
    }
    Ok(Pcm { rate, channels })
}

/// Adds one clip, already at the output rate and channel count.
fn add_clip(channels: &mut [Vec<f32>], clip: &AudioClip, source: &Pcm) {
    let rate = u64::from(source.rate);
    let total = channels.first().map_or(0, Vec::len) as u64;
    let (start, trim) = (clip.start_ms, clip.trim_start_ms);
    // First output sample with start_ms ≤ 1000·k / rate.
    let first = (start * rate).div_ceil(1000);
    let end = match clip.duration_ms {
        Some(d) => ((start + d) * rate).div_ceil(1000),
        None => u64::MAX,
    }
    .min(total);
    // Source sample for output sample k: k + floor((trim_start_ms − start_ms)·rate / 1000).
    let shift = (i128::from(trim) - i128::from(start)) * i128::from(rate);
    let shift = shift.div_euclid(1000) as i64;
    let length_ms = match clip.duration_ms {
        Some(d) => d as f64,
        None => source.len() as f64 * 1000.0 / rate as f64 - trim as f64,
    };

    for k in first..end {
        let Ok(j) = usize::try_from(k as i64 + shift) else {
            continue;
        };
        if j >= source.len() {
            if clip.duration_ms.is_none() {
                break;
            }
            continue;
        }
        let t = k as f64 * 1000.0 / rate as f64 - start as f64;
        let mut gain = 10f64.powf(value_at(&clip.gain_db, t) / 20.0);
        if clip.fade_in_ms > 0 {
            gain *= (t / clip.fade_in_ms as f64).min(1.0);
        }
        if clip.fade_out_ms > 0 {
            gain *= ((length_ms - t) / clip.fade_out_ms as f64).clamp(0.0, 1.0);
        }
        let gain = gain as f32;
        for (out, src) in channels.iter_mut().zip(&source.channels) {
            out[k as usize] += src[j] * gain;
        }
    }
}

/// Sound encoded as AAC-LC: the `AudioSpecificConfig` and the access units.
pub struct Aac {
    pub config: Vec<u8>,
    pub packets: Vec<Vec<u8>>,
}

/// Encodes sound as AAC-LC.
pub fn encode_aac(pcm: &Pcm) -> Result<Aac, String> {
    let channel_count = pcm.channels.len() as u16;
    let bitrate = if channel_count == 1 { 96_000 } else { 160_000 };
    let mut encoder = AacEncoder::new(AacEncoderConfig {
        bitrate_bps: bitrate,
        ..AacEncoderConfig::default()
    });
    let planes: Vec<&[f32]> = pcm.channels.iter().map(Vec::as_slice).collect();
    encoder
        .push_pcm_planar(&planes, pcm.rate)
        .map_err(|e| e.to_string())?;
    encoder.finish();
    let mut packets = Vec::new();
    while let Ok(packet) = encoder.next_packet() {
        packets.push(packet.data);
    }
    Ok(Aac {
        config: audio_specific_config_bytes(pcm.rate, channel_count),
        packets,
    })
}

/// The track description for encoded sound.
pub fn aac_track<'a>(pcm: &Pcm, aac: &'a Aac) -> mp4::AudioTrack<'a> {
    mp4::AudioTrack {
        sample_rate: pcm.rate,
        channels: pcm.channels.len() as u16,
        config: &aac.config,
        packets: &aac.packets,
        priming: AAC_PRIMING,
        length: pcm.len() as u64,
    }
}

/// Encodes sound as AAC-LC in an M4A file.
pub fn encode_m4a(pcm: &Pcm) -> Result<Vec<u8>, String> {
    let aac = encode_aac(pcm)?;
    Ok(mp4::write_m4a(&aac_track(pcm, &aac)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, rate: u32, n: usize, amp: f32) -> Vec<f32> {
        (0..n)
            .map(|i| {
                amp * (2.0 * std::f64::consts::PI * freq * i as f64 / f64::from(rate)).sin() as f32
            })
            .collect()
    }

    /// Signal-to-noise ratio of `got` against `want`, in dB.
    fn snr(want: &[f32], got: &[f32]) -> f64 {
        let signal: f64 = want.iter().map(|&s| f64::from(s).powi(2)).sum();
        let noise: f64 = want
            .iter()
            .zip(got)
            .map(|(&a, &b)| f64::from(a - b).powi(2))
            .sum();
        10.0 * (signal / noise).log10()
    }

    #[test]
    fn resampling_keeps_tones_and_timing() {
        let input = sine(1000.0, 44100, 44100, 0.5);
        let out = resample(&input, 44100, 48000);
        assert_eq!(out.len(), 48000);
        let want = sine(1000.0, 48000, 48000, 0.5);
        // Away from the edges, where the kernel runs off the signal.
        let quality = snr(&want[1000..47000], &out[1000..47000]);
        assert!(quality > 60.0, "{quality} dB");

        let down = resample(&want, 48000, 44100);
        assert_eq!(down.len(), 44100);
        assert!(snr(&input[1000..43000], &down[1000..43000]) > 60.0);

        // Tones above the new Nyquist frequency are removed.
        let high = sine(23000.0, 48000, 48000, 0.5);
        let gone = resample(&high, 48000, 22050);
        let energy: f32 = gone[1000..21000].iter().map(|s| s * s).sum::<f32>() / 20000.0;
        assert!(energy < 1e-4, "{energy}");
        assert_eq!(resample(&input, 44100, 44100), input);
    }

    #[test]
    fn channels_map_to_the_output() {
        let stereo = vec![vec![1.0, 0.0], vec![0.0, 1.0]];
        assert_eq!(map_channels(stereo, 1).unwrap(), [vec![0.5, 0.5]]);
        assert_eq!(
            map_channels(vec![vec![0.25]], 2).unwrap(),
            [vec![0.25], vec![0.25]]
        );
        assert!(map_channels(vec![vec![0.0]; 3], 2).is_err());
    }

    fn clip(extra: &str) -> AudioClip {
        let recipe = format!(
            r#"{{"unbaked": 0, "output": {{"kind": "audio", "duration_ms": 1000}},
                "assets": {{"a": {{"path": "assets/a.wav"}}}}, "layers": [],
                "audio": [{{"id": "c", "asset": "a"{extra}}}]}}"#
        );
        unbaked_core::recipe::parse(recipe.as_bytes())
            .unwrap()
            .audio
            .remove(0)
    }

    fn ones(rate: u32, n: usize) -> Pcm {
        Pcm {
            rate,
            channels: vec![vec![1.0; n]],
        }
    }

    #[test]
    fn clips_start_trim_fade_and_end() {
        let mut out = vec![vec![0.0f32; 20]];
        // 1000 samples per second: one sample per millisecond.
        add_clip(
            &mut out,
            &clip(r#", "start_ms": 5, "duration_ms": 10, "fade_in_ms": 4, "fade_out_ms": 2"#),
            &ones(1000, 100),
        );
        let got = &out[0];
        assert_eq!(got[4], 0.0);
        assert_eq!(&got[5..10], &[0.0, 0.25, 0.5, 0.75, 1.0]);
        assert_eq!(&got[12..15], &[1.0, 1.0, 0.5]);
        assert_eq!(got[15], 0.0);

        // Trimming shifts the source; past its end the clip is silent.
        let mut out = vec![vec![0.0f32; 10]];
        let ramp = Pcm {
            rate: 1000,
            channels: vec![(0..6).map(|i| i as f32).collect()],
        };
        add_clip(
            &mut out,
            &clip(r#", "start_ms": 2, "trim_start_ms": 3"#),
            &ramp,
        );
        assert_eq!(out[0], [0.0, 0.0, 3.0, 4.0, 5.0, 0.0, 0.0, 0.0, 0.0, 0.0]);

        // Gain in decibels.
        let mut out = vec![vec![0.0f32; 3]];
        add_clip(&mut out, &clip(r#", "gain_db": -20"#), &ones(1000, 3));
        assert!((out[0][1] - 0.1).abs() < 1e-6);
    }

    #[test]
    fn fractional_millisecond_starts_round_consistently() {
        // At 44.1 kHz, 1 ms is 44.1 samples: the clip starts at sample 45 with its first sample.
        let mut out = vec![vec![0.0f32; 50]];
        let ramp = Pcm {
            rate: 44100,
            channels: vec![(0..100).map(|i| i as f32).collect()],
        };
        add_clip(&mut out, &clip(r#", "start_ms": 1"#), &ramp);
        assert_eq!(out[0][44], 0.0);
        assert_eq!(&out[0][45..48], &[0.0, 1.0, 2.0]);
    }

    #[test]
    fn aac_round_trip_keeps_the_sound_in_place() {
        let rate = 48000;
        let n = 12_345;
        let left = sine(440.0, rate, n, 0.5);
        let right = sine(660.0, rate, n, 0.3);
        let pcm = Pcm {
            rate,
            channels: vec![left.clone(), right.clone()],
        };
        let m4a = encode_m4a(&pcm).unwrap();
        let decoded = decode(&m4a, 1_000_000).unwrap();
        assert_eq!(decoded.rate, rate);
        assert_eq!(
            decoded.len(),
            n,
            "the edit list hides the priming and padding"
        );
        // Skip the first frame, where the encoder starts from silence.
        let l = snr(&left[2048..n - 2048], &decoded.channels[0][2048..n - 2048]);
        let r = snr(&right[2048..n - 2048], &decoded.channels[1][2048..n - 2048]);
        assert!(l > 20.0 && r > 20.0, "left {l:.1} dB, right {r:.1} dB");
        // In place: shifting the decoded sound by a sample either way only makes it worse.
        let middle = 2048..n - 2048;
        let shifted = |lag: i64| -> f64 {
            let got: Vec<f32> = middle
                .clone()
                .map(|i| decoded.channels[0][(i as i64 + lag) as usize])
                .collect();
            snr(&left[middle.clone()], &got)
        };
        assert!(shifted(-1) < l && shifted(1) < l);

        let mono = Pcm {
            rate: 44100,
            channels: vec![sine(1000.0, 44100, 5000, 0.5)],
        };
        let decoded = decode(&encode_m4a(&mono).unwrap(), 1_000_000).unwrap();
        assert_eq!(
            (decoded.rate, decoded.channels.len(), decoded.len()),
            (44100, 1, 5000)
        );
    }

    #[test]
    fn wav_files_decode() {
        let samples: Vec<i16> = vec![0, 16384, -16384, 32767];
        let mut wav = b"RIFF".to_vec();
        wav.extend_from_slice(&(36 + 8u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&8000u32.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&8u32.to_le_bytes());
        for s in &samples {
            wav.extend_from_slice(&s.to_le_bytes());
        }
        let pcm = decode(&wav, 100).unwrap();
        assert_eq!(pcm.rate, 8000);
        assert_eq!(pcm.channels[0], [0.0, 0.5, -0.5, 32767.0 / 32768.0]);
        assert!(decode(&wav, 2).is_err());
        assert!(decode(b"not sound at all", 100).is_err());

        // Found by fuzzing: a huge channel count panicked inside symphonia.
        let mut wide = wav.clone();
        wide[22..24].copy_from_slice(&40_000u16.to_le_bytes());
        let error = decode(&wide, 100).unwrap_err();
        assert!(error.contains("40000 channels"), "{error}");
        let mut tagged = b"ID3\x04\0\0\0\0\0\0".to_vec();
        tagged.extend_from_slice(&wide);
        assert!(decode(&tagged, 100).is_err());
    }
}
