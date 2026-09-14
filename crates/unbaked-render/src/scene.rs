//! Drawing a recipe's layers at one moment (SPEC.md sections 4.5, 4.6, 4.10,
//! 4.11 and 5.4).
//!
//! Solid, image, text and group layers are drawn, with masks, effects,
//! transforms, opacity, blend modes, keyframes and transitions. Video layers
//! are refused with [`RenderError::Unsupported`] until they are built.

use std::collections::HashMap;
use std::rc::Rc;

use unbaked_core::json::join;
use unbaked_core::recipe::{AssetSource, Blend, Color, Content, Layer, Mask, MaskMode, Recipe};
use unbaked_core::sha256_hex;
use unbaked_core::sniff::{self, AssetKind};

use crate::draw::{Affine, composite, place};
use crate::effects;
use crate::image::{Pixmap, TooLarge, decode_jpeg, decode_png, premultiply};
use crate::motion::{transitions, value_at};
use crate::text::{self, TextError};
use crate::timing::{Moment, Span};
use crate::{FontSource, RenderError};

/// Limits a render stays within.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderLimits {
    /// Most pixels in any one buffer: the canvas, a group or mask buffer, a
    /// decoded image, a solid, or an image grown by effects.
    pub max_pixels: u64,
    /// Most samples per channel in any one sound buffer: a decoded source or
    /// the mix.
    pub max_samples: u64,
}

impl Default for RenderLimits {
    /// 64 million pixels, about 1 GiB per floating-point buffer, and 256
    /// million samples, 88 minutes of stereo at 48 kHz in about 2 GiB.
    fn default() -> Self {
        RenderLimits {
            max_pixels: 64_000_000,
            max_samples: 256_000_000,
        }
    }
}

fn straight(color: Color) -> [f32; 4] {
    [color.r, color.g, color.b, color.a].map(|c| f32::from(c) / 255.0)
}

struct Scene<'a> {
    recipe: &'a Recipe,
    file: &'a dyn Fn(&str) -> Option<&'a [u8]>,
    limits: RenderLimits,
    fonts: &'a dyn FontSource,
    decoded: HashMap<&'a str, Pixmap>,
    font_files: HashMap<&'a str, Rc<[u8]>>,
    moment: Moment,
    width: u32,
    height: u32,
}

/// Draws the frame of an `image` recipe at `output.at_ms`. `file` returns a
/// package file's bytes by path; `fonts` finds referenced fonts.
pub fn render_still<'a>(
    recipe: &'a Recipe,
    file: &'a dyn Fn(&str) -> Option<&'a [u8]>,
    fonts: &'a dyn FontSource,
    limits: RenderLimits,
) -> Result<Pixmap, RenderError> {
    let output = &recipe.output;
    let (Some(width), Some(height)) = (output.width, output.height) else {
        return Err(RenderError::Unsupported(
            "only image output is rendered yet".into(),
        ));
    };
    let background = output
        .background
        .map_or([0.0; 4], |c| premultiply(straight(c)));
    let mut canvas = Pixmap::filled(width, height, background, limits.max_pixels)
        .map_err(too_large("the canvas"))?;
    let mut scene = Scene {
        recipe,
        file,
        limits,
        fonts,
        decoded: HashMap::new(),
        font_files: HashMap::new(),
        moment: Moment::AtMs(output.at_ms),
        width,
        height,
    };
    scene.layers(
        &mut canvas,
        &recipe.layers,
        "/layers",
        Span::scene(output.duration_ms),
        Affine::IDENTITY,
    )?;
    Ok(canvas)
}

/// A layer's source image and how it maps to the layer box: pixel `(x, y)`
/// lands at box point `((x - origin_x) * scale_x, (y - origin_y) * scale_y)`.
struct Source {
    image: Pixmap,
    box_w: f64,
    box_h: f64,
    scale_x: f64,
    scale_y: f64,
    origin_x: i64,
    origin_y: i64,
}

impl Source {
    /// An image stretched onto its box.
    fn stretched(image: Pixmap, box_w: f64, box_h: f64) -> Source {
        Source {
            scale_x: box_w / f64::from(image.width),
            scale_y: box_h / f64::from(image.height),
            image,
            box_w,
            box_h,
            origin_x: 0,
            origin_y: 0,
        }
    }
}

fn too_large(what: &str) -> impl FnOnce(TooLarge) -> RenderError + '_ {
    move |e| RenderError::TooLarge {
        what: what.to_owned(),
        pixels: e.pixels,
        limit: e.limit,
    }
}

impl<'a> Scene<'a> {
    fn clear_canvas(&self, what: &str) -> Result<Pixmap, RenderError> {
        Pixmap::filled(self.width, self.height, [0.0; 4], self.limits.max_pixels)
            .map_err(too_large(what))
    }

    /// Draws a list of layers bottom first. `parent` is the enclosing span and
    /// `outer` the combined transform of the enclosing groups.
    fn layers(
        &mut self,
        target: &mut Pixmap,
        layers: &'a [Layer],
        path: &str,
        parent: Span,
        outer: Affine,
    ) -> Result<(), RenderError> {
        for (i, layer) in layers.iter().enumerate() {
            self.layer(target, layer, &join(path, &i.to_string()), parent, outer)?;
        }
        Ok(())
    }

    fn layer(
        &mut self,
        target: &mut Pixmap,
        layer: &'a Layer,
        path: &str,
        parent: Span,
        outer: Affine,
    ) -> Result<(), RenderError> {
        let span = parent.child(layer.start_ms, layer.end_ms);
        if layer.hidden || !span.visible_at(self.moment) {
            return Ok(());
        }
        let unsupported = |what: &str| {
            Err(RenderError::Unsupported(format!(
                "{path}: {what} are not rendered yet"
            )))
        };

        let t = span.local_ms(self.moment);
        let moved = transitions(
            layer.transition_in.as_ref(),
            layer.transition_out.as_ref(),
            t,
            span.length_ms(),
            f64::from(self.width),
            f64::from(self.height),
        );
        let opacity = (value_at(&layer.opacity, t) * moved.opacity).clamp(0.0, 1.0) as f32;
        let tf = &layer.transform;
        // The layer box is scaled, rotated, then its anchor placed at x, y (section 4.4).
        let own = |box_w: f64, box_h: f64| {
            Affine::translate(value_at(&tf.x, t) + moved.dx, value_at(&tf.y, t) + moved.dy)
                .then_after(Affine::rotate_deg(value_at(&tf.rotation_deg, t)))
                .then_after(Affine::scale(
                    value_at(&tf.scale_x, t) * moved.scale,
                    value_at(&tf.scale_y, t) * moved.scale,
                ))
                .then_after(Affine::translate(
                    -value_at(&tf.anchor_x, t) * box_w,
                    -value_at(&tf.anchor_y, t) * box_h,
                ))
        };

        // Steps 1-3 of section 5.4: the layer's pixels, in canvas space.
        let placed = match &layer.content {
            Content::Video { .. } => return unsupported("video layers"),
            Content::Group { layers } => {
                // The group box is the canvas; children render into an isolated buffer.
                let transform =
                    outer.then_after(own(f64::from(self.width), f64::from(self.height)));
                let mut buffer = self.clear_canvas(path)?;
                self.layers(&mut buffer, layers, &join(path, "layers"), span, transform)?;
                if !layer.effects.is_empty() {
                    // Group effects work in canvas space; what they push off the canvas is lost.
                    let grown = effects::apply(buffer, &layer.effects, t, self.limits.max_pixels)
                        .map_err(too_large(path))?;
                    buffer = self.crop_to_canvas(&grown, path)?;
                }
                buffer
            }
            Content::Solid { .. } | Content::Image { .. } | Content::Text(_) => {
                let source = self.source(layer, path)?;
                let grown = effects::apply(source.image, &layer.effects, t, self.limits.max_pixels)
                    .map_err(too_large(path))?;
                // Source pixels to the layer box; text ink and effect growth extend past the box.
                let to_canvas = outer
                    .then_after(own(source.box_w, source.box_h))
                    .then_after(Affine::scale(source.scale_x, source.scale_y))
                    .then_after(Affine::translate(
                        -(source.origin_x + grown.origin_x) as f64,
                        -(source.origin_y + grown.origin_y) as f64,
                    ));
                if layer.mask.is_none() {
                    place(target, &grown.image, to_canvas, opacity, layer.blend);
                    return Ok(());
                }
                let mut buffer = self.clear_canvas(path)?;
                place(&mut buffer, &grown.image, to_canvas, 1.0, Blend::Normal);
                buffer
            }
        };

        let mut placed = placed;
        // Step 4: the mask, placed by the enclosing groups but not by this layer's transform.
        if let Some(mask) = &layer.mask {
            let values = self.mask_values(mask, path, span, outer)?;
            for (px, m) in placed.data.as_chunks_mut::<4>().0.iter_mut().zip(values) {
                *px = px.map(|c| c * m);
            }
        }
        // Steps 5 and 6: opacity, then blend onto what is below.
        for (dst, src) in target
            .data
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(placed.data.as_chunks::<4>().0)
        {
            if src[3] > 0.0 {
                *dst = composite(layer.blend, *dst, src.map(|c| c * opacity));
            }
        }
        Ok(())
    }

    /// The per-pixel mask value `m` (section 4.11), with `invert` applied.
    fn mask_values(
        &mut self,
        mask: &'a Mask,
        path: &str,
        span: Span,
        outer: Affine,
    ) -> Result<Vec<f32>, RenderError> {
        let mask_path = join(&join(path, "mask"), "layers");
        let mut buffer = self.clear_canvas(&mask_path)?;
        self.layers(&mut buffer, &mask.layers, &mask_path, span, outer)?;
        Ok(buffer
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| {
                let m = match mask.mode {
                    MaskMode::Alpha => p[3],
                    // Luminance of straight colour times alpha, which is the same sum on premultiplied values.
                    MaskMode::Luminance => 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2],
                };
                let m = m.clamp(0.0, 1.0);
                if mask.invert { 1.0 - m } else { m }
            })
            .collect())
    }

    /// The canvas-sized part of an effected group buffer.
    fn crop_to_canvas(&self, grown: &effects::Effected, path: &str) -> Result<Pixmap, RenderError> {
        let mut out = self.clear_canvas(path)?;
        for y in 0..self.height {
            for x in 0..self.width {
                let p = grown
                    .image
                    .pixel_or_clear(i64::from(x) + grown.origin_x, i64::from(y) + grown.origin_y);
                out.set(x, y, p);
            }
        }
        Ok(out)
    }

    /// A solid's, image's or text's source image and layer box (section 4.6).
    fn source(&mut self, layer: &'a Layer, path: &str) -> Result<Source, RenderError> {
        match &layer.content {
            Content::Solid {
                color,
                width,
                height,
            } => {
                // A solid's source image is whole pixels, mapped onto its exact box.
                let fill = premultiply(straight(*color));
                let (w, h) = (width.ceil().max(1.0) as u32, height.ceil().max(1.0) as u32);
                let pixmap =
                    Pixmap::filled(w, h, fill, self.limits.max_pixels).map_err(too_large(path))?;
                Ok(Source::stretched(pixmap, *width, *height))
            }
            Content::Image {
                asset,
                width,
                height,
            } => {
                let image = self.image(asset)?;
                let (iw, ih) = (f64::from(image.width), f64::from(image.height));
                let (bw, bh) = match (width, height) {
                    (Some(w), Some(h)) => (*w, *h),
                    (Some(w), None) => (*w, w * ih / iw),
                    (None, Some(h)) => (h * iw / ih, *h),
                    (None, None) => (iw, ih),
                };
                Ok(Source::stretched(image.clone(), bw, bh))
            }
            Content::Text(text) => {
                let data = self.font(&text.font)?;
                let drawn =
                    text::draw(text, &data, self.limits.max_pixels).map_err(|e| match e {
                        TextError::Font(message) => RenderError::Decode {
                            asset: format!("font {:?}", text.font),
                            message,
                        },
                        TextError::TooLarge(e) => too_large(path)(e),
                    })?;
                Ok(Source {
                    image: drawn.image,
                    box_w: drawn.box_width,
                    box_h: drawn.box_height,
                    scale_x: 1.0,
                    scale_y: 1.0,
                    origin_x: drawn.origin_x,
                    origin_y: drawn.origin_y,
                })
            }
            _ => Err(RenderError::Unsupported(format!("{path}: no source image"))),
        }
    }

    /// A font asset's file, packed or found by its SHA-256 (section 4.12).
    fn font(&mut self, id: &'a str) -> Result<Rc<[u8]>, RenderError> {
        if let Some(data) = self.font_files.get(id) {
            return Ok(Rc::clone(data));
        }
        let missing = || RenderError::Unsupported(format!("asset {id:?} is missing"));
        let asset = self.recipe.assets.get(id).ok_or_else(missing)?;
        let data: Rc<[u8]> = match &asset.source {
            AssetSource::Path(path) => (self.file)(path).ok_or_else(missing)?.into(),
            AssetSource::Ref(reference) => self
                .fonts
                .find(&reference.sha256)
                .filter(|data| sha256_hex(data) == reference.sha256)
                .ok_or_else(|| RenderError::FontNotFound {
                    family: reference.family.clone(),
                    style: reference.style.clone(),
                })?
                .into(),
        };
        self.font_files.insert(id, Rc::clone(&data));
        Ok(data)
    }

    /// Decodes an image asset once and reuses it.
    fn image(&mut self, id: &'a str) -> Result<&Pixmap, RenderError> {
        if !self.decoded.contains_key(id) {
            let missing = || RenderError::Unsupported(format!("asset {id:?} is missing"));
            let asset = self.recipe.assets.get(id).ok_or_else(missing)?;
            let AssetSource::Path(path) = &asset.source else {
                return Err(missing());
            };
            let bytes = (self.file)(path).ok_or_else(missing)?;
            let head = &bytes[..bytes.len().min(sniff::HEADER_LEN)];
            let decoded = match sniff::detect(head) {
                Some(AssetKind::Png) => decode_png(bytes, self.limits.max_pixels),
                Some(AssetKind::Jpeg) => decode_jpeg(bytes, self.limits.max_pixels),
                _ => Err("not a PNG or JPEG image".into()),
            }
            .map_err(|message| RenderError::Decode {
                asset: path.clone(),
                message,
            })?;
            self.decoded.insert(id, decoded);
        }
        Ok(&self.decoded[id])
    }
}
