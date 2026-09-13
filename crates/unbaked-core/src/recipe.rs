//! `recipe.json`, SPEC.md section 4: typed structures and a reader that checks
//! the same structure as `schema/recipe.schema.json`. The rules the schema
//! cannot express are checked in [`crate::rules`].

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::json::{self, Problem, Problems, join};

/// A parsed recipe. Defaults from the spec are filled in.
#[derive(Debug, Clone, PartialEq)]
pub struct Recipe {
    pub output: Output,
    /// Asset id -> asset.
    pub assets: BTreeMap<String, Asset>,
    /// Visual layers, bottom first. Empty when an `audio` recipe has none.
    pub layers: Vec<Layer>,
    pub audio: Vec<AudioClip>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    Image,
    Audio,
    Video,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    pub kind: OutputKind,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// `None` means the kind's default (section 4.2).
    pub background: Option<Color>,
    pub at_ms: u64,
    pub fps: Option<Fps>,
    pub duration_ms: Option<u64>,
    pub sample_rate: u32,
    pub channels: u8,
}

/// A frame rate `num/den`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fps {
    pub num: u64,
    pub den: u64,
}

/// sRGB, not premultiplied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Asset {
    pub source: AssetSource,
    pub license: Option<License>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AssetSource {
    /// A package path under `assets/`.
    Path(String),
    /// A font that is not packed.
    Ref(FontRef),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FontRef {
    pub family: String,
    pub style: Option<String>,
    /// 64 lowercase hex characters.
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct License {
    pub spdx: Option<String>,
    pub file: Option<String>,
}

/// How a value moves from one key to the next. CSS keywords are stored as their curves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Ease {
    Linear,
    Hold,
    /// Cubic Bézier `[x1, y1, x2, y2]`.
    Bezier([f64; 4]),
}

impl Ease {
    pub const EASE: Ease = Ease::Bezier([0.25, 0.1, 0.25, 1.0]);
    pub const EASE_IN: Ease = Ease::Bezier([0.42, 0.0, 1.0, 1.0]);
    pub const EASE_OUT: Ease = Ease::Bezier([0.0, 0.0, 0.58, 1.0]);
    pub const EASE_IN_OUT: Ease = Ease::Bezier([0.42, 0.0, 0.58, 1.0]);
}

#[derive(Debug, Clone, PartialEq)]
pub struct Key {
    pub t_ms: u64,
    pub v: f64,
    pub ease: Ease,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Animatable {
    Constant(f64),
    /// At least one key.
    Keys(Vec<Key>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Transform {
    pub x: Animatable,
    pub y: Animatable,
    pub anchor_x: Animatable,
    pub anchor_y: Animatable,
    pub scale_x: Animatable,
    pub scale_y: Animatable,
    pub rotation_deg: Animatable,
}

impl Default for Transform {
    fn default() -> Self {
        let zero = Animatable::Constant(0.0);
        let one = Animatable::Constant(1.0);
        Transform {
            x: zero.clone(),
            y: zero.clone(),
            anchor_x: zero.clone(),
            anchor_y: zero.clone(),
            scale_x: one.clone(),
            scale_y: one,
            rotation_deg: zero,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blend {
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionKind {
    Fade,
    SlideLeft,
    SlideRight,
    SlideUp,
    SlideDown,
    Zoom,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Transition {
    pub kind: TransitionKind,
    pub duration_ms: u64,
    pub ease: Ease,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Blur {
        sigma: Animatable,
    },
    Shadow {
        dx: Animatable,
        dy: Animatable,
        sigma: Animatable,
        color: Color,
    },
    Adjust {
        brightness: Animatable,
        contrast: Animatable,
        saturation: Animatable,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaskMode {
    Alpha,
    Luminance,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Mask {
    pub layers: Vec<Layer>,
    pub mode: MaskMode,
    pub invert: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    pub id: String,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub transform: Transform,
    pub opacity: Animatable,
    pub blend: Blend,
    pub effects: Vec<Effect>,
    pub mask: Option<Box<Mask>>,
    pub transition_in: Option<Transition>,
    pub transition_out: Option<Transition>,
    pub hidden: bool,
    pub content: Content,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    Image {
        asset: String,
        width: Option<f64>,
        height: Option<f64>,
    },
    Video {
        asset: String,
        width: Option<f64>,
        height: Option<f64>,
        trim_start_ms: u64,
    },
    Text(Text),
    Solid {
        color: Color,
        width: f64,
        height: f64,
    },
    Group {
        layers: Vec<Layer>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Text {
    pub text: String,
    pub font: String,
    pub size_px: f64,
    pub color: Color,
    pub line_height: f64,
    pub align: Align,
    pub box_width: Option<f64>,
    pub font_index: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioClip {
    pub id: String,
    pub asset: String,
    pub start_ms: u64,
    pub trim_start_ms: u64,
    pub duration_ms: Option<u64>,
    pub gain_db: Animatable,
    pub fade_in_ms: u64,
    pub fade_out_ms: u64,
    pub muted: bool,
}

/// Reads `recipe.json`, checking its structure. Returns every problem found, up
/// to [`json::MAX_PROBLEMS`].
pub fn parse(bytes: &[u8]) -> Result<Recipe, Vec<Problem>> {
    let value = json::parse(bytes).map_err(|p| vec![p])?;
    parse_value(&value)
}

/// Like [`parse`], for JSON that is already loaded.
pub fn parse_value(value: &Value) -> Result<Recipe, Vec<Problem>> {
    let mut p = Problems::default();
    match recipe(&mut p, value) {
        Ok(r) if p.is_empty() => Ok(r),
        _ => Err(p.into_vec()),
    }
}

/// A problem was already recorded.
type R<T> = Result<T, ()>;

fn opt<T>(
    p: &mut Problems,
    map: &Map<String, Value>,
    path: &str,
    key: &str,
    read: impl FnOnce(&mut Problems, &Value, &str) -> Option<T>,
) -> R<Option<T>> {
    match map.get(key) {
        None => Ok(None),
        Some(v) => read(p, v, &join(path, key)).map(Some).ok_or(()),
    }
}

fn req<T>(
    p: &mut Problems,
    map: &Map<String, Value>,
    path: &str,
    key: &str,
    read: impl FnOnce(&mut Problems, &Value, &str) -> Option<T>,
) -> R<T> {
    let v = p.required(map, path, key).ok_or(())?;
    read(p, v, &join(path, key)).ok_or(())
}

fn time(p: &mut Problems, v: &Value, path: &str) -> Option<u64> {
    p.integer(v, path, 0)
}

fn positive_int(p: &mut Problems, v: &Value, path: &str) -> Option<u64> {
    p.integer(v, path, 1)
}

fn positive_number(p: &mut Problems, v: &Value, path: &str) -> Option<f64> {
    let n = p.number(v, path)?;
    if n > 0.0 {
        Some(n)
    } else {
        p.add(path, "must be greater than 0");
        None
    }
}

fn string(p: &mut Problems, v: &Value, path: &str) -> Option<String> {
    p.string(v, path).map(str::to_owned)
}

fn boolean(p: &mut Problems, v: &Value, path: &str) -> Option<bool> {
    p.boolean(v, path)
}

pub(crate) fn is_id(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn id(p: &mut Problems, v: &Value, path: &str) -> Option<String> {
    let s = p.string(v, path)?;
    if is_id(s) {
        Some(s.to_owned())
    } else {
        p.add(
            path,
            format!("{s:?} is not a valid id (1-64 letters, digits, _ or -)"),
        );
        None
    }
}

fn color(p: &mut Problems, v: &Value, path: &str) -> Option<Color> {
    let s = p.string(v, path)?;
    let parsed = parse_color(s);
    if parsed.is_none() {
        p.add(path, format!("{s:?} is not a color (#RRGGBB or #RRGGBBAA)"));
    }
    parsed
}

pub(crate) fn parse_color(s: &str) -> Option<Color> {
    let hex = s.strip_prefix('#')?;
    if !(hex.len() == 6 || hex.len() == 8) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some(Color {
        r: byte(0)?,
        g: byte(2)?,
        b: byte(4)?,
        a: if hex.len() == 8 { byte(6)? } else { 255 },
    })
}

/// A package path under `assets/` that also passes the section 3.2 name rules.
pub(crate) fn package_path(p: &mut Problems, v: &Value, path: &str) -> Option<String> {
    let s = p.string(v, path)?;
    let under_assets = s
        .strip_prefix("assets/")
        .is_some_and(|rest| !rest.is_empty() && !rest.ends_with('/'));
    if !under_assets {
        p.add(path, format!("{s:?} must be a file path under assets/"));
        return None;
    }
    if let Err(e) = crate::package::check_name(s) {
        p.add(path, e.to_string());
        return None;
    }
    Some(s.to_owned())
}

fn recipe(p: &mut Problems, v: &Value) -> R<Recipe> {
    let map = p
        .object(v, "", &["unbaked", "output", "assets", "layers", "audio"])
        .ok_or(())?;

    let version = req(p, map, "", "unbaked", |p, v, path| {
        let n = p.integer(v, path, 0)?;
        if n == u64::from(crate::SPEC_VERSION) {
            Some(n)
        } else {
            p.add(
                path,
                format!(
                    "spec version {n} is not supported; this reader implements version {}",
                    crate::SPEC_VERSION
                ),
            );
            None
        }
    });
    let output = req(p, map, "", "output", |p, v, path| output(p, v, path).ok());
    let assets = req(p, map, "", "assets", |p, v, path| assets(p, v, path).ok());

    let needs_layers = matches!(&output, Ok(o) if o.kind != OutputKind::Audio);
    let layers = if needs_layers {
        req(p, map, "", "layers", |p, v, path| {
            layer_list(p, v, path).ok()
        })
        .map(Some)
    } else {
        opt(p, map, "", "layers", |p, v, path| {
            layer_list(p, v, path).ok()
        })
    };
    let audio = opt(p, map, "", "audio", |p, v, path| {
        list(p, v, path, |p, v, path| audio_clip(p, v, path).ok())
    });

    version?;
    Ok(Recipe {
        output: output?,
        assets: assets?,
        layers: layers?.unwrap_or_default(),
        audio: audio?.unwrap_or_default(),
    })
}

/// Reads every item of an array, reporting problems in all of them.
fn list<T>(
    p: &mut Problems,
    v: &Value,
    path: &str,
    mut read: impl FnMut(&mut Problems, &Value, &str) -> Option<T>,
) -> Option<Vec<T>> {
    let Some(items) = v.as_array() else {
        p.add(path, format!("expected an array, found {}", json::kind(v)));
        return None;
    };
    let mut out = Vec::with_capacity(items.len());
    let mut ok = true;
    for (i, item) in items.iter().enumerate() {
        match read(p, item, &join(path, &i.to_string())) {
            Some(t) => out.push(t),
            None => ok = false,
        }
    }
    ok.then_some(out)
}

fn output(p: &mut Problems, v: &Value, path: &str) -> R<Output> {
    const FIELDS: &[&str] = &[
        "kind",
        "width",
        "height",
        "background",
        "at_ms",
        "fps",
        "duration_ms",
        "sample_rate",
        "channels",
    ];
    let map = p.object(v, path, FIELDS).ok_or(())?;
    let kind = req(p, map, path, "kind", |p, v, path| {
        p.choice(
            v,
            path,
            &[
                ("image", OutputKind::Image),
                ("audio", OutputKind::Audio),
                ("video", OutputKind::Video),
            ],
        )
    });
    let size = |p: &mut Problems, v: &Value, path: &str| {
        let n = p.integer(v, path, 1)?;
        if n <= 16384 {
            Some(n as u32)
        } else {
            p.add(path, "must be at most 16384");
            None
        }
    };
    let width = opt(p, map, path, "width", size);
    let height = opt(p, map, path, "height", size);
    let background = opt(p, map, path, "background", color);
    let at_ms = opt(p, map, path, "at_ms", time);
    let fps = opt(p, map, path, "fps", fps);
    let duration_ms = opt(p, map, path, "duration_ms", positive_int);
    let sample_rate = opt(p, map, path, "sample_rate", |p, v, path| {
        let n = p.integer(v, path, 0)?;
        if n == 44100 || n == 48000 {
            Some(n as u32)
        } else {
            p.add(path, "must be 44100 or 48000");
            None
        }
    });
    let channels = opt(p, map, path, "channels", |p, v, path| {
        let n = p.integer(v, path, 0)?;
        if n == 1 || n == 2 {
            Some(n as u8)
        } else {
            p.add(path, "must be 1 or 2");
            None
        }
    });

    let kind = kind?;
    let needed: &[&str] = match kind {
        OutputKind::Image => &["width", "height"],
        OutputKind::Audio => &["duration_ms"],
        OutputKind::Video => &["width", "height", "fps", "duration_ms"],
    };
    for key in needed {
        if !map.contains_key(*key) {
            p.add(
                path,
                format!("missing required field {key:?} for this kind"),
            );
        }
    }
    let (w, h) = (width?, height?);
    if kind == OutputKind::Video {
        for (key, n) in [("width", w), ("height", h)] {
            if n.is_some_and(|n| n % 2 != 0) {
                p.add(&join(path, key), "must be even for video");
            }
        }
    }
    Ok(Output {
        kind,
        width: w,
        height: h,
        background: background?,
        at_ms: at_ms?.unwrap_or(0),
        fps: fps?,
        duration_ms: duration_ms?,
        sample_rate: sample_rate?.unwrap_or(48000),
        channels: channels?.unwrap_or(2),
    })
}

fn fps(p: &mut Problems, v: &Value, path: &str) -> Option<Fps> {
    let s = p.string(v, path)?;
    let part = |t: &str| {
        let digits = !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) && !t.starts_with('0');
        if digits { t.parse::<u64>().ok() } else { None }
    };
    let parsed = match s.split_once('/') {
        None => part(s).map(|num| Fps { num, den: 1 }),
        Some((a, b)) => part(a).zip(part(b)).map(|(num, den)| Fps { num, den }),
    };
    if parsed.is_none() {
        p.add(
            path,
            format!("{s:?} is not a frame rate (\"30\" or \"30000/1001\")"),
        );
    }
    parsed
}

fn assets(p: &mut Problems, v: &Value, path: &str) -> R<BTreeMap<String, Asset>> {
    let Some(map) = v.as_object() else {
        p.add(path, format!("expected an object, found {}", json::kind(v)));
        return Err(());
    };
    let mut out = BTreeMap::new();
    let mut ok = true;
    for (key, value) in map {
        let item_path = join(path, key);
        if !is_id(key) {
            p.add(
                &item_path,
                format!("{key:?} is not a valid id (1-64 letters, digits, _ or -)"),
            );
            ok = false;
        }
        match asset(p, value, &item_path) {
            Ok(a) => {
                out.insert(key.clone(), a);
            }
            Err(()) => ok = false,
        }
    }
    if ok { Ok(out) } else { Err(()) }
}

fn asset(p: &mut Problems, v: &Value, path: &str) -> R<Asset> {
    let map = p.object(v, path, &["path", "ref", "license"]).ok_or(())?;
    let file = opt(p, map, path, "path", package_path);
    let font = opt(p, map, path, "ref", |p, v, path| font_ref(p, v, path).ok());
    let license = opt(p, map, path, "license", |p, v, path| {
        license(p, v, path).ok()
    });
    let source = match (map.contains_key("path"), map.contains_key("ref")) {
        (true, true) => {
            p.add(path, "has both \"path\" and \"ref\"; use exactly one");
            Err(())
        }
        (false, false) => {
            p.add(path, "needs \"path\" or \"ref\"");
            Err(())
        }
        (true, false) => file.and_then(|f| f.ok_or(())).map(AssetSource::Path),
        (false, true) => font.and_then(|f| f.ok_or(())).map(AssetSource::Ref),
    };
    Ok(Asset {
        source: source?,
        license: license?,
    })
}

fn font_ref(p: &mut Problems, v: &Value, path: &str) -> R<FontRef> {
    let map = p
        .object(v, path, &["family", "style", "sha256"])
        .ok_or(())?;
    let family = req(p, map, path, "family", |p, v, path| {
        let s = p.string(v, path)?;
        if s.is_empty() {
            p.add(path, "must not be empty");
            None
        } else {
            Some(s.to_owned())
        }
    });
    let style = opt(p, map, path, "style", string);
    let sha256 = req(p, map, path, "sha256", |p, v, path| {
        let s = p.string(v, path)?;
        if json::is_sha256_hex(s) {
            Some(s.to_owned())
        } else {
            p.add(path, "must be 64 lowercase hex characters");
            None
        }
    });
    Ok(FontRef {
        family: family?,
        style: style?,
        sha256: sha256?,
    })
}

fn license(p: &mut Problems, v: &Value, path: &str) -> R<License> {
    let map = p.object(v, path, &["spdx", "file"]).ok_or(())?;
    let spdx = opt(p, map, path, "spdx", |p, v, path| {
        let s = p.string(v, path)?;
        let ok = !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'));
        if ok {
            Some(s.to_owned())
        } else {
            p.add(path, format!("{s:?} is not an SPDX id or LicenseRef- name"));
            None
        }
    });
    let file = opt(p, map, path, "file", package_path);
    let (spdx, file) = (spdx?, file?);
    let has_spdx = map.contains_key("spdx");
    if !has_spdx && !map.contains_key("file") {
        p.add(path, "needs \"spdx\", \"file\" or both");
        return Err(());
    }
    if matches!(spdx.as_deref(), Some("OFL-1.1" | "Apache-2.0")) && file.is_none() {
        p.add(
            path,
            "OFL-1.1 and Apache-2.0 require \"file\": the licence text must travel with the font",
        );
        return Err(());
    }
    Ok(License { spdx, file })
}

fn ease(p: &mut Problems, v: &Value, path: &str) -> Option<Ease> {
    if let Some(items) = v.as_array() {
        if items.len() != 4 {
            p.add(
                path,
                "a cubic Bézier needs exactly 4 numbers [x1, y1, x2, y2]",
            );
            return None;
        }
        let mut n = [0.0; 4];
        let mut ok = true;
        for (i, item) in items.iter().enumerate() {
            let item_path = join(path, &i.to_string());
            match p.number(item, &item_path) {
                Some(x) if i % 2 == 0 && !(0.0..=1.0).contains(&x) => {
                    p.add(&item_path, "x1 and x2 must be between 0 and 1");
                    ok = false;
                }
                Some(x) => n[i] = x,
                None => ok = false,
            }
        }
        return ok.then_some(Ease::Bezier(n));
    }
    if !v.is_string() {
        p.add(
            path,
            format!(
                "expected an easing name or [x1, y1, x2, y2], found {}",
                json::kind(v)
            ),
        );
        return None;
    }
    p.choice(
        v,
        path,
        &[
            ("linear", Ease::Linear),
            ("hold", Ease::Hold),
            ("ease", Ease::EASE),
            ("ease-in", Ease::EASE_IN),
            ("ease-out", Ease::EASE_OUT),
            ("ease-in-out", Ease::EASE_IN_OUT),
        ],
    )
}

fn animatable(p: &mut Problems, v: &Value, path: &str) -> Option<Animatable> {
    if let Some(n) = v.as_f64() {
        return Some(Animatable::Constant(n));
    }
    if !v.is_object() {
        p.add(
            path,
            format!(
                "expected a number or a keyframe object, found {}",
                json::kind(v)
            ),
        );
        return None;
    }
    let map = p.object(v, path, &["keys"])?;
    let keys = req(p, map, path, "keys", |p, v, path| {
        let keys = list(p, v, path, |p, v, path| key(p, v, path).ok())?;
        if keys.is_empty() && v.as_array().is_some() {
            p.add(path, "needs at least one key");
            return None;
        }
        Some(keys)
    })
    .ok()?;
    Some(Animatable::Keys(keys))
}

fn key(p: &mut Problems, v: &Value, path: &str) -> R<Key> {
    let map = p.object(v, path, &["t_ms", "v", "ease"]).ok_or(())?;
    let t_ms = req(p, map, path, "t_ms", time);
    let value = req(p, map, path, "v", |p, v, path| p.number(v, path));
    let e = opt(p, map, path, "ease", ease);
    Ok(Key {
        t_ms: t_ms?,
        v: value?,
        ease: e?.unwrap_or(Ease::Linear),
    })
}

fn transform(p: &mut Problems, v: &Value, path: &str) -> R<Transform> {
    const FIELDS: &[&str] = &[
        "x",
        "y",
        "anchor_x",
        "anchor_y",
        "scale_x",
        "scale_y",
        "rotation_deg",
    ];
    let map = p.object(v, path, FIELDS).ok_or(())?;
    let d = Transform::default();
    let mut get = |key: &str, default: Animatable| {
        opt(p, map, path, key, animatable).map(|a| a.unwrap_or(default))
    };
    let x = get("x", d.x);
    let y = get("y", d.y);
    let anchor_x = get("anchor_x", d.anchor_x);
    let anchor_y = get("anchor_y", d.anchor_y);
    let scale_x = get("scale_x", d.scale_x);
    let scale_y = get("scale_y", d.scale_y);
    let rotation_deg = get("rotation_deg", d.rotation_deg);
    Ok(Transform {
        x: x?,
        y: y?,
        anchor_x: anchor_x?,
        anchor_y: anchor_y?,
        scale_x: scale_x?,
        scale_y: scale_y?,
        rotation_deg: rotation_deg?,
    })
}

fn transition(p: &mut Problems, v: &Value, path: &str) -> R<Transition> {
    let map = p
        .object(v, path, &["type", "duration_ms", "ease"])
        .ok_or(())?;
    let kind = req(p, map, path, "type", |p, v, path| {
        p.choice(
            v,
            path,
            &[
                ("fade", TransitionKind::Fade),
                ("slide-left", TransitionKind::SlideLeft),
                ("slide-right", TransitionKind::SlideRight),
                ("slide-up", TransitionKind::SlideUp),
                ("slide-down", TransitionKind::SlideDown),
                ("zoom", TransitionKind::Zoom),
            ],
        )
    });
    let duration_ms = req(p, map, path, "duration_ms", positive_int);
    let e = opt(p, map, path, "ease", |p, v, path| {
        let e = ease(p, v, path)?;
        if e == Ease::Hold {
            p.add(path, "\"hold\" is not allowed for transitions");
            return None;
        }
        Some(e)
    });
    Ok(Transition {
        kind: kind?,
        duration_ms: duration_ms?,
        ease: e?.unwrap_or(Ease::Linear),
    })
}

#[derive(Clone, Copy)]
enum EffectKind {
    Blur,
    Shadow,
    Adjust,
}

fn effect(p: &mut Problems, v: &Value, path: &str) -> R<Effect> {
    let Some(map) = v.as_object() else {
        p.add(path, format!("expected an object, found {}", json::kind(v)));
        return Err(());
    };
    let kind = req(p, map, path, "type", |p, v, path| {
        p.choice(
            v,
            path,
            &[
                ("blur", EffectKind::Blur),
                ("shadow", EffectKind::Shadow),
                ("adjust", EffectKind::Adjust),
            ],
        )
    })?;
    let fields: &[&str] = match kind {
        EffectKind::Blur => &["type", "sigma"],
        EffectKind::Shadow => &["type", "dx", "dy", "sigma", "color"],
        EffectKind::Adjust => &["type", "brightness", "contrast", "saturation"],
    };
    p.object(v, path, fields);
    let mut num = |key: &str, default: f64| {
        opt(p, map, path, key, animatable).map(|a| a.unwrap_or(Animatable::Constant(default)))
    };
    match kind {
        EffectKind::Blur => {
            let sigma = req(p, map, path, "sigma", animatable)?;
            Ok(Effect::Blur { sigma })
        }
        EffectKind::Shadow => {
            let dx = num("dx", 0.0);
            let dy = num("dy", 0.0);
            let sigma = num("sigma", 0.0);
            let shadow_color = opt(p, map, path, "color", color);
            Ok(Effect::Shadow {
                dx: dx?,
                dy: dy?,
                sigma: sigma?,
                color: shadow_color?.unwrap_or(Color {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 0x80,
                }),
            })
        }
        EffectKind::Adjust => {
            let brightness = num("brightness", 1.0);
            let contrast = num("contrast", 1.0);
            let saturation = num("saturation", 1.0);
            Ok(Effect::Adjust {
                brightness: brightness?,
                contrast: contrast?,
                saturation: saturation?,
            })
        }
    }
}

fn mask(p: &mut Problems, v: &Value, path: &str) -> R<Mask> {
    let map = p.object(v, path, &["layers", "mode", "invert"]).ok_or(())?;
    let layers = req(p, map, path, "layers", |p, v, path| {
        layer_list(p, v, path).ok()
    });
    let mode = opt(p, map, path, "mode", |p, v, path| {
        p.choice(
            v,
            path,
            &[
                ("alpha", MaskMode::Alpha),
                ("luminance", MaskMode::Luminance),
            ],
        )
    });
    let invert = opt(p, map, path, "invert", boolean);
    Ok(Mask {
        layers: layers?,
        mode: mode?.unwrap_or(MaskMode::Alpha),
        invert: invert?.unwrap_or(false),
    })
}

fn layer_list(p: &mut Problems, v: &Value, path: &str) -> R<Vec<Layer>> {
    list(p, v, path, |p, v, path| layer(p, v, path).ok()).ok_or(())
}

#[derive(Clone, Copy)]
enum LayerKind {
    Image,
    Video,
    Text,
    Solid,
    Group,
}

const LAYER_FIELDS: &[&str] = &[
    "id",
    "type",
    "start_ms",
    "end_ms",
    "transform",
    "opacity",
    "blend",
    "effects",
    "mask",
    "in",
    "out",
    "hidden",
];

fn layer(p: &mut Problems, v: &Value, path: &str) -> R<Layer> {
    let Some(map) = v.as_object() else {
        p.add(path, format!("expected an object, found {}", json::kind(v)));
        return Err(());
    };
    let kind = req(p, map, path, "type", |p, v, path| {
        p.choice(
            v,
            path,
            &[
                ("image", LayerKind::Image),
                ("video", LayerKind::Video),
                ("text", LayerKind::Text),
                ("solid", LayerKind::Solid),
                ("group", LayerKind::Group),
            ],
        )
    });
    if let Ok(kind) = kind {
        let own: &[&str] = match kind {
            LayerKind::Image => &["asset", "width", "height"],
            LayerKind::Video => &["asset", "width", "height", "trim_start_ms"],
            LayerKind::Text => &[
                "text",
                "font",
                "size_px",
                "color",
                "line_height",
                "align",
                "box_width",
                "font_index",
            ],
            LayerKind::Solid => &["color", "width", "height"],
            LayerKind::Group => &["layers"],
        };
        let allowed: Vec<&str> = LAYER_FIELDS.iter().chain(own).copied().collect();
        p.object(v, path, &allowed);
    }

    let layer_id = req(p, map, path, "id", id);
    let start_ms = opt(p, map, path, "start_ms", time);
    let end_ms = opt(p, map, path, "end_ms", time);
    let layer_transform = opt(p, map, path, "transform", |p, v, path| {
        transform(p, v, path).ok()
    });
    let opacity = opt(p, map, path, "opacity", animatable);
    let blend = opt(p, map, path, "blend", |p, v, path| {
        p.choice(
            v,
            path,
            &[
                ("normal", Blend::Normal),
                ("multiply", Blend::Multiply),
                ("screen", Blend::Screen),
                ("overlay", Blend::Overlay),
                ("darken", Blend::Darken),
                ("lighten", Blend::Lighten),
                ("color-dodge", Blend::ColorDodge),
                ("color-burn", Blend::ColorBurn),
                ("hard-light", Blend::HardLight),
                ("soft-light", Blend::SoftLight),
                ("difference", Blend::Difference),
                ("exclusion", Blend::Exclusion),
            ],
        )
    });
    let effects = opt(p, map, path, "effects", |p, v, path| {
        list(p, v, path, |p, v, path| effect(p, v, path).ok())
    });
    let layer_mask = opt(p, map, path, "mask", |p, v, path| mask(p, v, path).ok());
    let transition_in = opt(p, map, path, "in", |p, v, path| transition(p, v, path).ok());
    let transition_out = opt(p, map, path, "out", |p, v, path| {
        transition(p, v, path).ok()
    });
    let hidden = opt(p, map, path, "hidden", boolean);

    let content = match kind? {
        LayerKind::Image => {
            let asset = req(p, map, path, "asset", id);
            let width = opt(p, map, path, "width", positive_number);
            let height = opt(p, map, path, "height", positive_number);
            Content::Image {
                asset: asset?,
                width: width?,
                height: height?,
            }
        }
        LayerKind::Video => {
            let asset = req(p, map, path, "asset", id);
            let width = opt(p, map, path, "width", positive_number);
            let height = opt(p, map, path, "height", positive_number);
            let trim = opt(p, map, path, "trim_start_ms", time);
            Content::Video {
                asset: asset?,
                width: width?,
                height: height?,
                trim_start_ms: trim?.unwrap_or(0),
            }
        }
        LayerKind::Text => {
            let text = req(p, map, path, "text", string);
            let font = req(p, map, path, "font", id);
            let size_px = req(p, map, path, "size_px", positive_number);
            let fill = opt(p, map, path, "color", color);
            let line_height = opt(p, map, path, "line_height", positive_number);
            let align = opt(p, map, path, "align", |p, v, path| {
                p.choice(
                    v,
                    path,
                    &[
                        ("left", Align::Left),
                        ("center", Align::Center),
                        ("right", Align::Right),
                    ],
                )
            });
            let box_width = opt(p, map, path, "box_width", positive_number);
            let font_index = opt(p, map, path, "font_index", |p, v, path| {
                let n = p.integer(v, path, 0)?;
                u32::try_from(n).ok().or_else(|| {
                    p.add(path, "is too large");
                    None
                })
            });
            Content::Text(Text {
                text: text?,
                font: font?,
                size_px: size_px?,
                color: fill?.unwrap_or(Color {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 255,
                }),
                line_height: line_height?.unwrap_or(1.2),
                align: align?.unwrap_or(Align::Left),
                box_width: box_width?,
                font_index: font_index?.unwrap_or(0),
            })
        }
        LayerKind::Solid => {
            let fill = req(p, map, path, "color", color);
            let width = req(p, map, path, "width", positive_number);
            let height = req(p, map, path, "height", positive_number);
            Content::Solid {
                color: fill?,
                width: width?,
                height: height?,
            }
        }
        LayerKind::Group => {
            let layers = req(p, map, path, "layers", |p, v, path| {
                layer_list(p, v, path).ok()
            });
            Content::Group { layers: layers? }
        }
    };

    Ok(Layer {
        id: layer_id?,
        start_ms: start_ms?.unwrap_or(0),
        end_ms: end_ms?,
        transform: layer_transform?.unwrap_or_default(),
        opacity: opacity?.unwrap_or(Animatable::Constant(1.0)),
        blend: blend?.unwrap_or(Blend::Normal),
        effects: effects?.unwrap_or_default(),
        mask: layer_mask?.map(Box::new),
        transition_in: transition_in?,
        transition_out: transition_out?,
        hidden: hidden?.unwrap_or(false),
        content,
    })
}

fn audio_clip(p: &mut Problems, v: &Value, path: &str) -> R<AudioClip> {
    const FIELDS: &[&str] = &[
        "id",
        "asset",
        "start_ms",
        "trim_start_ms",
        "duration_ms",
        "gain_db",
        "fade_in_ms",
        "fade_out_ms",
        "muted",
    ];
    let map = p.object(v, path, FIELDS).ok_or(())?;
    let clip_id = req(p, map, path, "id", id);
    let asset = req(p, map, path, "asset", id);
    let start_ms = opt(p, map, path, "start_ms", time);
    let trim = opt(p, map, path, "trim_start_ms", time);
    let duration_ms = opt(p, map, path, "duration_ms", positive_int);
    let gain_db = opt(p, map, path, "gain_db", animatable);
    let fade_in = opt(p, map, path, "fade_in_ms", time);
    let fade_out = opt(p, map, path, "fade_out_ms", time);
    let muted = opt(p, map, path, "muted", boolean);
    Ok(AudioClip {
        id: clip_id?,
        asset: asset?,
        start_ms: start_ms?.unwrap_or(0),
        trim_start_ms: trim?.unwrap_or(0),
        duration_ms: duration_ms?,
        gain_db: gain_db?.unwrap_or(Animatable::Constant(0.0)),
        fade_in_ms: fade_in?.unwrap_or(0),
        fade_out_ms: fade_out?.unwrap_or(0),
        muted: muted?.unwrap_or(false),
    })
}
