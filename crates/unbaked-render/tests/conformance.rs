//! The conformance cases in `tests/conformance/` (SPEC.md section 8).
//!
//! Each case's `package/` is rendered before encoding and compared with
//! `expected/` using section 8's tolerances. `UNBAKED_BLESS=1` rewrites
//! `expected/` from this renderer instead.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use unbaked_core::package::Limits;
use unbaked_core::recipe::{self, AssetSource, Content, Layer, OutputKind, Recipe};
use unbaked_core::{pack, rules, sha256_hex, sniff};
use unbaked_render::scene::Frames;
use unbaked_render::sound::{self, Pcm};
use unbaked_render::timing::{Moment, frame_count};
use unbaked_render::{FontSource, RenderLimits, image, text};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/conformance");
const FONTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fonts");

/// Most channel difference allowed for a pixel to match.
const PIXEL_TOLERANCE: u8 = 2;
/// Most glyph position difference allowed, in pixels.
const GLYPH_TOLERANCE: f64 = 0.5;
/// Least ratio of reference signal to difference signal, in dB.
const AUDIO_SNR_DB: f64 = 60.0;

/// Referenced fonts: any file in `tests/fonts/` with the right SHA-256.
struct FontFolder;

impl FontSource for FontFolder {
    fn find(&self, sha256: &str) -> Option<Vec<u8>> {
        fs::read_dir(FONTS)
            .ok()?
            .filter_map(|entry| fs::read(entry.ok()?.path()).ok())
            .find(|data| sha256_hex(data) == sha256)
    }
}

/// One expected file.
enum Output {
    /// 8-bit RGBA: straight colour for `image`, over opaque black for `video`.
    Frame {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    /// Pixels covered by text layers, excluded from frame comparison.
    Cover {
        width: u32,
        height: u32,
        covered: Vec<bool>,
    },
    /// Glyph positions in layer box pixels, by text layer id.
    Glyphs(Value),
    Audio(Pcm),
}

#[test]
fn conformance_cases_match_their_expected_output() {
    let bless = std::env::var_os("UNBAKED_BLESS").is_some();
    let mut cases: Vec<PathBuf> = fs::read_dir(ROOT)
        .expect("tests/conformance exists")
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.join("package").is_dir())
        .collect();
    cases.sort();
    assert!(cases.len() >= 10, "found only {} cases", cases.len());

    let mut failures = Vec::new();
    for case in &cases {
        let name = case.file_name().unwrap().to_string_lossy();
        let result = render_case(case).and_then(|outputs| {
            if bless {
                bless_case(case, &outputs)
            } else {
                compare_case(case, &outputs)
            }
        });
        if let Err(e) = result {
            failures.push(format!("{name}: {e}"));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

fn render_case(case: &Path) -> Result<BTreeMap<String, Output>, String> {
    let files = pack::read_folder(&case.join("package"), Limits::default())
        .map_err(|e| format!("package: {e}"))?;
    let recipe_json = files.get("recipe.json").ok_or("no recipe.json")?;
    let recipe = recipe::parse(recipe_json).map_err(|p| format!("recipe: {p:?}"))?;
    let kind_of = |path: &str| {
        files
            .get(path)
            .map(|d| sniff::detect(&d[..d.len().min(sniff::HEADER_LEN)]))
    };
    let problems = rules::check(&recipe, &kind_of);
    if !problems.is_empty() {
        return Err(format!("recipe: {problems:?}"));
    }
    let file = |path: &str| files.get(path).map(Vec::as_slice);
    let limits = RenderLimits::default();
    let mut outputs = BTreeMap::new();

    let mut text_layers = Vec::new();
    collect_text(&recipe.layers, &mut text_layers);
    let moments: Vec<(String, Moment)> = match recipe.output.kind {
        OutputKind::Image => vec![("0000".into(), Moment::AtMs(recipe.output.at_ms))],
        OutputKind::Video => {
            let fps = recipe.output.fps.ok_or("no fps")?;
            let frames = frame_count(recipe.output.duration_ms.ok_or("no duration")?, fps);
            (0..frames)
                .map(|n| (format!("{n:04}"), Moment::Frame { n, fps }))
                .collect()
        }
        OutputKind::Audio => Vec::new(),
    };
    if !moments.is_empty() {
        let mut frames =
            Frames::new(&recipe, &file, &FontFolder, limits).map_err(|e| e.to_string())?;
        for (number, moment) in moments {
            let canvas = frames.draw(moment).map_err(|e| e.to_string())?;
            let rgba = if recipe.output.kind == OutputKind::Video {
                over_black(&canvas.data)
            } else {
                canvas.to_rgba8()
            };
            let (width, height) = (canvas.width, canvas.height);
            if !text_layers.is_empty() {
                let covered = frames.text_cover(moment).map_err(|e| e.to_string())?;
                let cover = Output::Cover {
                    width,
                    height,
                    covered,
                };
                outputs.insert(format!("cover-{number}.png"), cover);
            }
            let frame = Output::Frame {
                width,
                height,
                rgba,
            };
            outputs.insert(format!("frame-{number}.png"), frame);
        }
    }
    if !text_layers.is_empty() {
        outputs.insert(
            "glyphs.json".into(),
            Output::Glyphs(glyphs(&recipe, &files, &text_layers)?),
        );
    }
    if !recipe.audio.is_empty() {
        let pcm = sound::mix(&recipe, &file, limits).map_err(|e| e.to_string())?;
        outputs.insert("audio.wav".into(), Output::Audio(pcm));
    }
    Ok(outputs)
}

/// Every text layer, in groups and masks too.
fn collect_text<'a>(layers: &'a [Layer], out: &mut Vec<&'a Layer>) {
    for layer in layers {
        match &layer.content {
            Content::Text(_) => out.push(layer),
            Content::Group { layers } => collect_text(layers, out),
            _ => {}
        }
        if let Some(mask) = &layer.mask {
            collect_text(&mask.layers, out);
        }
    }
}

/// Section 5.7 for video: premultiplied colour is the colour over opaque black.
fn over_black(data: &[f32]) -> Vec<u8> {
    let quantise = |v: f32| (v * 255.0 + 0.5).floor().clamp(0.0, 255.0) as u8;
    data.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [quantise(p[0]), quantise(p[1]), quantise(p[2]), 255])
        .collect()
}

/// Each text layer's glyphs: id and origin in layer box pixels, x right and y
/// down from the box's top-left corner, rounded to 1/1000 px.
fn glyphs(recipe: &Recipe, files: &pack::Files, layers: &[&Layer]) -> Result<Value, String> {
    let mut out = serde_json::Map::new();
    for layer in layers {
        let Content::Text(t) = &layer.content else {
            unreachable!("only text layers are collected");
        };
        let data = match &recipe.assets[&t.font].source {
            AssetSource::Path(path) => files[path].clone(),
            AssetSource::Ref(r) => FontFolder.find(&r.sha256).ok_or("font not found")?,
        };
        let font = text::font(&data, t.font_index).map_err(|e| format!("{e:?}"))?;
        let layout = text::layout(t, &font).map_err(|e| format!("{e:?}"))?;
        let round = |v: f64| (v * 1000.0).round() / 1000.0;
        let placed: Vec<Value> = layout
            .lines
            .iter()
            .flat_map(|line| {
                line.glyphs.iter().map(move |g| {
                    json!({
                        "glyph": g.id,
                        "x": round(line.x + g.x as f64 * layout.scale),
                        "y": round(line.baseline - g.y as f64 * layout.scale),
                    })
                })
            })
            .collect();
        out.insert(layer.id.clone(), placed.into());
    }
    Ok(json!({ "layers": out }))
}

fn bless_case(case: &Path, outputs: &BTreeMap<String, Output>) -> Result<(), String> {
    let expected = case.join("expected");
    if expected.exists() {
        fs::remove_dir_all(&expected).map_err(|e| e.to_string())?;
    }
    fs::create_dir_all(&expected).map_err(|e| e.to_string())?;
    for (name, output) in outputs {
        let bytes = match output {
            Output::Frame {
                width,
                height,
                rgba,
            } => image::encode_png(*width, *height, rgba)?,
            Output::Cover {
                width,
                height,
                covered,
            } => {
                let rgba: Vec<u8> = covered
                    .iter()
                    .flat_map(|&c| if c { [255; 4] } else { [0; 4] })
                    .collect();
                image::encode_png(*width, *height, &rgba)?
            }
            Output::Glyphs(value) => {
                let mut text = serde_json::to_vec_pretty(value).unwrap();
                text.push(b'\n');
                text
            }
            Output::Audio(pcm) => write_wav(pcm),
        };
        fs::write(expected.join(name), bytes).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn compare_case(case: &Path, outputs: &BTreeMap<String, Output>) -> Result<(), String> {
    let expected = case.join("expected");
    let mut names: Vec<String> = fs::read_dir(&expected)
        .map_err(|e| format!("expected/: {e}"))?
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let produced: Vec<&String> = outputs.keys().collect();
    if names.iter().collect::<Vec<_>>() != produced {
        return Err(format!("expected files {names:?}, rendered {produced:?}"));
    }
    let read = |name: &str| fs::read(expected.join(name)).map_err(|e| format!("{name}: {e}"));
    let mut problems = Vec::new();
    for (name, output) in outputs {
        let result = match output {
            Output::Frame {
                width,
                height,
                rgba,
            } => {
                let reference = read_png(&read(name)?)?;
                let cover_name = name.replace("frame-", "cover-");
                let excluded = if outputs.contains_key(&cover_name) {
                    let (_, _, cover) = read_png(&read(&cover_name)?)?;
                    cover.as_chunks::<4>().0.iter().map(|p| p[3] > 0).collect()
                } else {
                    Vec::new()
                };
                compare_frames(reference, (*width, *height, rgba), &excluded)
            }
            Output::Cover { .. } => Ok(()),
            Output::Glyphs(value) => serde_json::from_slice(&read(name)?)
                .map_err(|e| e.to_string())
                .and_then(|reference| compare_glyphs(&reference, value)),
            Output::Audio(pcm) => read_wav(&read(name)?).and_then(|r| compare_audio(&r, pcm)),
        };
        if let Err(e) = result {
            problems.push(format!("{name}: {e}"));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

/// Section 8: every channel within ±2 for at least 99.9% of the pixels not
/// covered by text.
fn compare_frames(
    (rw, rh, reference): (u32, u32, Vec<u8>),
    (width, height, rgba): (u32, u32, &[u8]),
    excluded: &[bool],
) -> Result<(), String> {
    if (rw, rh) != (width, height) {
        return Err(format!("size {width}x{height}, expected {rw}x{rh}"));
    }
    let (mut counted, mut off, mut worst) = (0u64, 0u64, 0u8);
    for (i, (r, p)) in reference
        .as_chunks::<4>()
        .0
        .iter()
        .zip(rgba.as_chunks::<4>().0)
        .enumerate()
    {
        if excluded.get(i).copied().unwrap_or(false) {
            continue;
        }
        counted += 1;
        let diff = r.iter().zip(p).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
        worst = worst.max(diff);
        if diff > PIXEL_TOLERANCE {
            off += 1;
        }
    }
    if off * 1000 > counted {
        return Err(format!(
            "{off} of {counted} pixels differ by more than {PIXEL_TOLERANCE} (worst {worst})"
        ));
    }
    Ok(())
}

/// Section 8: the same glyphs, each within 0.5 px of the reference.
fn compare_glyphs(reference: &Value, glyphs: &Value) -> Result<(), String> {
    let layers = |v: &Value| v["layers"].as_object().cloned().unwrap_or_default();
    let (reference, glyphs) = (layers(reference), layers(glyphs));
    let ids =
        |layers: &serde_json::Map<String, Value>| layers.keys().cloned().collect::<BTreeSet<_>>();
    if ids(&reference) != ids(&glyphs) {
        return Err("different text layers".into());
    }
    for (id, want) in &reference {
        let (want, got) = (want.as_array().unwrap(), glyphs[id].as_array().unwrap());
        let ids = |list: &[Value]| list.iter().map(|g| g["glyph"].clone()).collect::<Vec<_>>();
        if ids(want) != ids(got) {
            return Err(format!("layer {id}: different glyphs"));
        }
        for (i, (w, g)) in want.iter().zip(got).enumerate() {
            let dx = (w["x"].as_f64().unwrap() - g["x"].as_f64().unwrap()).abs();
            let dy = (w["y"].as_f64().unwrap() - g["y"].as_f64().unwrap()).abs();
            if dx > GLYPH_TOLERANCE || dy > GLYPH_TOLERANCE {
                return Err(format!("layer {id}: glyph {i} is off by ({dx}, {dy}) px"));
            }
        }
    }
    Ok(())
}

/// Section 8: the difference signal at least 60 dB below the reference.
fn compare_audio(reference: &Pcm, pcm: &Pcm) -> Result<(), String> {
    if reference.rate != pcm.rate
        || reference.channels.len() != pcm.channels.len()
        || reference.len() != pcm.len()
    {
        return Err(format!(
            "{} Hz, {} channels, {} samples; expected {} Hz, {} channels, {} samples",
            pcm.rate,
            pcm.channels.len(),
            pcm.len(),
            reference.rate,
            reference.channels.len(),
            reference.len()
        ));
    }
    let (mut signal, mut noise) = (0.0f64, 0.0f64);
    for (r, p) in reference.channels.iter().zip(&pcm.channels) {
        for (a, b) in r.iter().zip(p) {
            signal += f64::from(*a).powi(2);
            noise += f64::from(a - b).powi(2);
        }
    }
    if noise * 10f64.powf(AUDIO_SNR_DB / 10.0) > signal {
        let db = 10.0 * (signal / noise).log10();
        return Err(format!(
            "the difference is only {db:.1} dB below the reference"
        ));
    }
    Ok(())
}

/// 8-bit RGBA from a PNG file.
fn read_png(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buffer = vec![0; reader.output_buffer_size().ok_or("PNG too large")?];
    let info = reader.next_frame(&mut buffer).map_err(|e| e.to_string())?;
    if (info.color_type, info.bit_depth) != (png::ColorType::Rgba, png::BitDepth::Eight) {
        return Err("reference PNGs must be 8-bit RGBA".into());
    }
    buffer.truncate(info.buffer_size());
    Ok((info.width, info.height, buffer))
}

/// A WAV file of 32-bit float samples, channels interleaved.
fn write_wav(pcm: &Pcm) -> Vec<u8> {
    let channels = pcm.channels.len() as u16;
    let data_len = (pcm.len() * pcm.channels.len() * 4) as u32;
    let mut out = Vec::with_capacity(58 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(50 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&18u32.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&pcm.rate.to_le_bytes());
    out.extend_from_slice(&(pcm.rate * u32::from(channels) * 4).to_le_bytes());
    out.extend_from_slice(&(channels * 4).to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(b"fact");
    out.extend_from_slice(&4u32.to_le_bytes());
    out.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for k in 0..pcm.len() {
        for channel in &pcm.channels {
            out.extend_from_slice(&channel[k].to_le_bytes());
        }
    }
    out
}

/// Reads a WAV file written by [`write_wav`].
fn read_wav(bytes: &[u8]) -> Result<Pcm, String> {
    let bad = || "not a 32-bit float WAV file".to_string();
    if bytes.get(..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        return Err(bad());
    }
    let u16_at = |i: usize| {
        bytes
            .get(i..i + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let u32_at = |i: usize| {
        bytes
            .get(i..i + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let (mut pos, mut format, mut data) = (12, None, None);
    while let (Some(kind), Some(len)) = (bytes.get(pos..pos + 4), u32_at(pos + 4)) {
        let body = pos + 8..pos + 8 + len as usize;
        match kind {
            b"fmt " => {
                format = Some((
                    u16_at(body.start),
                    u16_at(body.start + 2),
                    u32_at(body.start + 4),
                    u16_at(body.start + 14),
                ))
            }
            b"data" => data = bytes.get(body.clone()),
            _ => {}
        }
        pos = body.end + body.end % 2;
    }
    let (Some((Some(3), Some(channels), Some(rate), Some(32))), Some(data)) = (format, data) else {
        return Err(bad());
    };
    let channels = usize::from(channels);
    let samples: Vec<f32> = data
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    Ok(Pcm {
        rate,
        channels: (0..channels)
            .map(|c| samples.iter().skip(c).step_by(channels).copied().collect())
            .collect(),
    })
}

#[test]
fn frames_match_within_the_section_8_tolerances() {
    let reference = vec![100u8; 4 * 2000];
    let frame = |changes: &[(usize, u8)]| {
        let mut rgba = reference.clone();
        for &(i, v) in changes {
            rgba[i] = v;
        }
        rgba
    };
    let compare = |rgba: &[u8], excluded: &[bool]| {
        compare_frames((40, 50, reference.clone()), (40, 50, rgba), excluded)
    };
    // ±2 everywhere matches.
    assert!(compare(&vec![102; 8000], &[]).is_ok());
    assert!(compare(&vec![98; 8000], &[]).is_ok());
    // Two of 2000 pixels off by 3 is 0.1%: still a match. Three is not.
    assert!(compare(&frame(&[(0, 103), (5, 97)]), &[]).is_ok());
    assert!(compare(&frame(&[(0, 103), (5, 97), (11, 0)]), &[]).is_err());
    // Alpha counts too.
    assert!(compare(&frame(&[(3, 0), (7, 0), (11, 0)]), &[]).is_err());
    // Pixels covered by text are not compared.
    let mut excluded = vec![false; 2000];
    excluded[..3].fill(true);
    assert!(compare(&frame(&[(0, 0), (5, 0), (11, 0)]), &excluded).is_ok());
    assert!(compare_frames((40, 50, reference.clone()), (50, 40, &reference), &[]).is_err());
}

#[test]
fn glyphs_match_within_half_a_pixel() {
    let layout =
        |x: f64, glyph: u32| json!({"layers": {"t": [{"glyph": glyph, "x": x, "y": 10.0}]}});
    assert!(compare_glyphs(&layout(3.0, 5), &layout(3.5, 5)).is_ok());
    assert!(compare_glyphs(&layout(3.0, 5), &layout(3.501, 5)).is_err());
    assert!(compare_glyphs(&layout(3.0, 5), &layout(3.0, 6)).is_err());
    assert!(compare_glyphs(&layout(3.0, 5), &json!({"layers": {}})).is_err());
}

#[test]
fn audio_matches_when_the_difference_is_60_db_down() {
    let tone: Vec<f32> = (0..4800).map(|k| (k as f32 * 0.05).sin() * 0.5).collect();
    let pcm = |scale: f32| Pcm {
        rate: 48000,
        channels: vec![tone.iter().map(|s| s * scale).collect()],
    };
    // A gain change of 0.05% is 66 dB down; 0.2% is 54 dB down.
    assert!(compare_audio(&pcm(1.0), &pcm(1.0005)).is_ok());
    assert!(compare_audio(&pcm(1.0), &pcm(1.002)).is_err());
    let mut short = pcm(1.0);
    short.channels[0].pop();
    assert!(compare_audio(&pcm(1.0), &short).is_err());
    let wav = write_wav(&pcm(1.0));
    assert!(compare_audio(&read_wav(&wav).unwrap(), &pcm(1.0)).is_ok());
}
