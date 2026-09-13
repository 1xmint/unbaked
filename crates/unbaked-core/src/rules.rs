//! The recipe rules JSON Schema cannot express, listed in the schema's
//! `$comment` and in SPEC.md section 4. Run after [`crate::recipe::parse`].

use std::collections::HashMap;

use crate::json::{Problem, Problems, join};
use crate::recipe::{
    Animatable, AssetSource, Content, Ease, Effect, Layer, OutputKind, Recipe, Transition,
    TransitionKind,
};
use crate::sniff::AssetKind;

/// Groups may nest this deep and no deeper (section 4.10).
pub const MAX_GROUP_DEPTH: usize = 32;

/// What the rules need to know about the package.
pub trait Files {
    /// `None` if the package has no such file, otherwise its detected kind.
    fn kind(&self, path: &str) -> Option<Option<AssetKind>>;
}

impl<F: Fn(&str) -> Option<Option<AssetKind>>> Files for F {
    fn kind(&self, path: &str) -> Option<Option<AssetKind>> {
        self(path)
    }
}

/// Checks a parsed recipe against the package it came from. Returns every
/// problem found, up to [`crate::json::MAX_PROBLEMS`].
pub fn check(recipe: &Recipe, files: &impl Files) -> Vec<Problem> {
    let mut c = Checker {
        recipe,
        files,
        p: Problems::default(),
        ids: HashMap::new(),
    };
    c.assets();
    let output = &recipe.output;
    if output.kind == OutputKind::Image
        && let Some(duration) = output.duration_ms
        && output.at_ms >= duration
    {
        c.p.add(
            "/output/at_ms",
            format!("must be less than output.duration_ms ({duration})"),
        );
    }
    let top = Span {
        start: 0,
        end: output.duration_ms.map(u128::from),
    };
    c.layers(&recipe.layers, "/layers", top, 0);
    for (i, clip) in recipe.audio.iter().enumerate() {
        let path = join("/audio", &i.to_string());
        c.unique_id(&clip.id, &path);
        c.uses_asset(&clip.asset, &join(&path, "asset"), Use::Audio);
        c.keys_increase(&clip.gain_db, &join(&path, "gain_db"));
    }
    c.p.into_vec()
}

/// A layer's absolute visible time, section 5.2. `end` is `None` when it never ends.
#[derive(Clone, Copy)]
struct Span {
    start: u128,
    end: Option<u128>,
}

#[derive(Clone, Copy)]
enum Use {
    Image,
    Video,
    Font,
    Audio,
}

struct Checker<'a, F> {
    recipe: &'a Recipe,
    files: &'a F,
    p: Problems,
    /// Layer or clip id -> where it was first used.
    ids: HashMap<&'a str, String>,
}

impl<'a, F: Files> Checker<'a, F> {
    fn assets(&mut self) {
        for (id, asset) in &self.recipe.assets {
            let path = join("/assets", id);
            if let AssetSource::Path(file) = &asset.source {
                match self.files.kind(file) {
                    None => self.p.add(
                        &join(&path, "path"),
                        format!("the package has no file {file:?}"),
                    ),
                    Some(Some(AssetKind::Font)) if asset.license.is_none() => self
                        .p
                        .add(&path, "a packed font needs a \"license\" (section 4.12)"),
                    Some(_) => {}
                }
            }
            if let Some(file) = asset.license.as_ref().and_then(|l| l.file.as_ref())
                && self.files.kind(file).is_none()
            {
                self.p.add(
                    &join(&join(&path, "license"), "file"),
                    format!("the package has no file {file:?}"),
                );
            }
        }
    }

    fn unique_id(&mut self, id: &'a str, path: &str) {
        if let Some(first) = self.ids.get(id) {
            let message = format!("id {id:?} is already used at {first}");
            self.p.add(&join(path, "id"), message);
        } else {
            self.ids.insert(id, path.to_owned());
        }
    }

    fn uses_asset(&mut self, id: &str, path: &str, used_as: Use) {
        let Some(asset) = self.recipe.assets.get(id) else {
            self.p.add(path, format!("there is no asset {id:?}"));
            return;
        };
        let wanted = match used_as {
            Use::Image => "a PNG or JPEG image",
            Use::Video => "an MP4 video",
            Use::Font => "a TrueType or OpenType font",
            Use::Audio => "an audio file (M4A, MP4, MP3, WAV or FLAC)",
        };
        let file = match &asset.source {
            AssetSource::Ref(_) if matches!(used_as, Use::Font) => return,
            AssetSource::Ref(_) => {
                self.p.add(
                    path,
                    format!("asset {id:?} is a referenced font, not {wanted}"),
                );
                return;
            }
            AssetSource::Path(file) => file,
        };
        // A missing file is already reported under /assets.
        let Some(kind) = self.files.kind(file) else {
            return;
        };
        let fits = kind.is_some_and(|k| match used_as {
            Use::Image => k.is_image(),
            Use::Video => k.is_video(),
            Use::Font => k.is_font(),
            Use::Audio => k.is_audio(),
        });
        if !fits {
            self.p
                .add(path, format!("asset {id:?} ({file}) is not {wanted}"));
        }
    }

    fn layers(&mut self, layers: &'a [Layer], path: &str, parent: Span, depth: usize) {
        for (i, layer) in layers.iter().enumerate() {
            self.layer(layer, &join(path, &i.to_string()), parent, depth);
        }
    }

    fn layer(&mut self, layer: &'a Layer, path: &str, parent: Span, depth: usize) {
        self.unique_id(&layer.id, path);

        if let Some(end) = layer.end_ms
            && layer.start_ms >= end
        {
            self.p.add(
                &join(path, "end_ms"),
                format!("must be greater than start_ms ({})", layer.start_ms),
            );
        }
        let start = parent.start + u128::from(layer.start_ms);
        let own_end = layer.end_ms.map(|e| parent.start + u128::from(e));
        let end = match (own_end, parent.end) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let span = Span { start, end };

        if let Some(t) = &layer.transition_in {
            self.transition(t, &join(path, "in"), span);
        }
        if let Some(t) = &layer.transition_out {
            if end.is_none() {
                self.p.add(
                    &join(path, "out"),
                    "a layer with \"out\" needs an end: end_ms on it or an enclosing layer, or output.duration_ms",
                );
            }
            self.transition(t, &join(path, "out"), span);
        }

        let t = &layer.transform;
        let transform_path = join(path, "transform");
        for (name, value) in [
            ("x", &t.x),
            ("y", &t.y),
            ("anchor_x", &t.anchor_x),
            ("anchor_y", &t.anchor_y),
            ("scale_x", &t.scale_x),
            ("scale_y", &t.scale_y),
            ("rotation_deg", &t.rotation_deg),
        ] {
            self.keys_increase(value, &join(&transform_path, name));
        }
        for (name, value) in [("scale_x", &t.scale_x), ("scale_y", &t.scale_y)] {
            if range(value).0 < 0.0 {
                self.p.add(
                    &join(&transform_path, name),
                    "must not be negative at any time, including between keys",
                );
            }
        }
        self.keys_increase(&layer.opacity, &join(path, "opacity"));

        for (i, effect) in layer.effects.iter().enumerate() {
            self.effect(effect, &join(&join(path, "effects"), &i.to_string()));
        }

        match &layer.content {
            Content::Image { asset, .. } => {
                self.uses_asset(asset, &join(path, "asset"), Use::Image)
            }
            Content::Video { asset, .. } => {
                self.uses_asset(asset, &join(path, "asset"), Use::Video)
            }
            Content::Text(text) => self.uses_asset(&text.font, &join(path, "font"), Use::Font),
            Content::Solid { .. } => {}
            Content::Group { layers } => {
                if depth + 1 > MAX_GROUP_DEPTH {
                    self.p.add(
                        path,
                        format!("groups nest more than {MAX_GROUP_DEPTH} deep"),
                    );
                } else {
                    self.layers(layers, &join(path, "layers"), span, depth + 1);
                }
            }
        }

        if let Some(mask) = &layer.mask {
            let mask_path = join(&join(path, "mask"), "layers");
            self.layers(&mask.layers, &mask_path, span, depth);
        }
    }

    fn transition(&mut self, t: &Transition, path: &str, span: Span) {
        if let Some(end) = span.end {
            let length = end.saturating_sub(span.start);
            if u128::from(t.duration_ms) > length {
                self.p.add(
                    &join(path, "duration_ms"),
                    format!("is longer than the layer's visible length ({length} ms)"),
                );
            }
        }
        if t.kind == TransitionKind::Zoom && ease_range(t.ease).0 < 0.0 {
            self.p.add(
                &join(path, "ease"),
                "this curve dips below 0, which would make the zoom scale negative",
            );
        }
    }

    fn effect(&mut self, effect: &Effect, path: &str) {
        match effect {
            Effect::Blur { sigma } => {
                self.keys_increase(sigma, &join(path, "sigma"));
                if range(sigma).0 <= 0.0 {
                    self.p
                        .add(&join(path, "sigma"), "must be greater than 0 at all times");
                }
            }
            Effect::Shadow { dx, dy, sigma, .. } => {
                self.keys_increase(dx, &join(path, "dx"));
                self.keys_increase(dy, &join(path, "dy"));
                self.keys_increase(sigma, &join(path, "sigma"));
                if range(sigma).0 < 0.0 {
                    self.p
                        .add(&join(path, "sigma"), "must not be negative at any time");
                }
            }
            Effect::Adjust {
                brightness,
                contrast,
                saturation,
            } => {
                self.keys_increase(brightness, &join(path, "brightness"));
                self.keys_increase(contrast, &join(path, "contrast"));
                self.keys_increase(saturation, &join(path, "saturation"));
            }
        }
    }

    fn keys_increase(&mut self, value: &Animatable, path: &str) {
        let Animatable::Keys(keys) = value else {
            return;
        };
        for (i, pair) in keys.windows(2).enumerate() {
            if pair[1].t_ms <= pair[0].t_ms {
                let key_path = join(&join(path, "keys"), &(i + 1).to_string());
                self.p.add(
                    &join(&key_path, "t_ms"),
                    format!(
                        "must be greater than the previous key's t_ms ({})",
                        pair[0].t_ms
                    ),
                );
            }
        }
    }
}

/// The lowest and highest value an animatable number takes at any time.
pub fn range(value: &Animatable) -> (f64, f64) {
    match value {
        Animatable::Constant(v) => (*v, *v),
        Animatable::Keys(keys) => {
            let mut lo = f64::INFINITY;
            let mut hi = f64::NEG_INFINITY;
            for k in keys {
                lo = lo.min(k.v);
                hi = hi.max(k.v);
            }
            for pair in keys.windows(2) {
                let (e_lo, e_hi) = ease_range(pair[0].ease);
                let delta = pair[1].v - pair[0].v;
                for e in [e_lo, e_hi] {
                    let v = pair[0].v + delta * e;
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
            }
            (lo, hi)
        }
    }
}

/// The lowest and highest output of an easing curve over progress 0 to 1.
pub fn ease_range(ease: Ease) -> (f64, f64) {
    let [_, y1, _, y2] = match ease {
        Ease::Linear | Ease::Hold => return (0.0, 1.0),
        Ease::Bezier(points) => points,
    };
    if (0.0..=1.0).contains(&y1) && (0.0..=1.0).contains(&y2) {
        // The curve stays inside the hull of its control points.
        return (0.0, 1.0);
    }
    // x1, x2 in [0, 1] make x rise steadily, so progress 0..1 covers the whole
    // curve and its extremes in y are at the ends or where dy/du = 0.
    let y = |u: f64| {
        let v = 1.0 - u;
        3.0 * v * v * u * y1 + 3.0 * v * u * u * y2 + u * u * u
    };
    let (d0, d1, d2) = (y1, y2 - y1, 1.0 - y2);
    let a = d0 - 2.0 * d1 + d2;
    let b = 2.0 * (d1 - d0);
    let c = d0;
    let mut roots = Vec::new();
    if a.abs() < 1e-12 {
        if b.abs() > 1e-12 {
            roots.push(-c / b);
        }
    } else {
        let disc = b * b - 4.0 * a * c;
        if disc >= 0.0 {
            let s = disc.sqrt();
            roots.push((-b + s) / (2.0 * a));
            roots.push((-b - s) / (2.0 * a));
        }
    }
    let mut lo: f64 = 0.0;
    let mut hi: f64 = 1.0;
    for u in roots.into_iter().filter(|u| (0.0..=1.0).contains(u)) {
        lo = lo.min(y(u));
        hi = hi.max(y(u));
    }
    (lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe::Key;

    fn key(t_ms: u64, v: f64, ease: Ease) -> Key {
        Key { t_ms, v, ease }
    }

    #[test]
    fn standard_curves_stay_between_0_and_1() {
        for ease in [
            Ease::Linear,
            Ease::Hold,
            Ease::EASE,
            Ease::EASE_IN,
            Ease::EASE_OUT,
            Ease::EASE_IN_OUT,
        ] {
            assert_eq!(ease_range(ease), (0.0, 1.0));
        }
    }

    #[test]
    fn overshooting_curves_report_their_extremes() {
        // Control points below 0 pull the curve under 0 early on; it still ends at 1.
        let (lo, hi) = ease_range(Ease::Bezier([0.3, -1.0, 0.7, -1.0]));
        assert!(lo < -0.5 && lo > -1.0, "{lo}");
        assert_eq!(hi, 1.0);
        let (lo, hi) = ease_range(Ease::Bezier([0.3, 2.0, 0.7, 2.0]));
        assert_eq!(lo, 0.0);
        assert!(hi > 1.5 && hi < 2.0, "{hi}");
    }

    #[test]
    fn range_follows_keys_and_overshoot() {
        let plain = Animatable::Keys(vec![key(0, 1.0, Ease::Linear), key(10, 0.0, Ease::Linear)]);
        assert_eq!(range(&plain), (0.0, 1.0));
        let overshoot = Animatable::Keys(vec![
            key(0, 1.0, Ease::Bezier([0.3, 2.0, 0.7, 2.0])),
            key(10, 0.0, Ease::Linear),
        ]);
        assert!(range(&overshoot).0 < 0.0);
        assert_eq!(range(&Animatable::Constant(-2.0)), (-2.0, -2.0));
    }
}
