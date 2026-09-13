//! Drawing a recipe's layers at one moment (SPEC.md sections 4.5, 4.6 and 5.4).
//!
//! This version draws solid and image layers with transforms, opacity, blend
//! modes, keyframes and transitions. Groups, masks, effects, text and video
//! layers are refused with [`RenderError::Unsupported`] until they are built.

use std::collections::HashMap;

use unbaked_core::json::join;
use unbaked_core::recipe::{AssetSource, Color, Content, Layer, Recipe};
use unbaked_core::sniff::{self, AssetKind};

use crate::RenderError;
use crate::draw::{Affine, place};
use crate::image::{Pixmap, decode_jpeg, decode_png, premultiply};
use crate::motion::{transitions, value_at};
use crate::timing::{Moment, Span};

/// Limits a render stays within.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderLimits {
    /// Most pixels in any one buffer: the canvas, a decoded image, or a solid.
    pub max_pixels: u64,
}

impl Default for RenderLimits {
    /// 64 million pixels, about 1 GiB per floating-point buffer.
    fn default() -> Self {
        RenderLimits {
            max_pixels: 64_000_000,
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
    decoded: HashMap<&'a str, Pixmap>,
    moment: Moment,
    width: f64,
    height: f64,
}

/// Draws the frame of an `image` recipe at `output.at_ms`. `file` returns a
/// package file's bytes by path.
pub fn render_still<'a>(
    recipe: &'a Recipe,
    file: &'a dyn Fn(&str) -> Option<&'a [u8]>,
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
    let mut canvas = Pixmap::filled(width, height, background, limits.max_pixels).map_err(|e| {
        RenderError::TooLarge {
            what: "the canvas".into(),
            pixels: e.pixels,
            limit: e.limit,
        }
    })?;
    let mut scene = Scene {
        recipe,
        file,
        limits,
        decoded: HashMap::new(),
        moment: Moment::AtMs(output.at_ms),
        width: f64::from(width),
        height: f64::from(height),
    };
    let top = Span::scene(output.duration_ms);
    for (i, layer) in recipe.layers.iter().enumerate() {
        scene.layer(&mut canvas, layer, &join("/layers", &i.to_string()), top)?;
    }
    Ok(canvas)
}

impl<'a> Scene<'a> {
    fn layer(
        &mut self,
        canvas: &mut Pixmap,
        layer: &'a Layer,
        path: &str,
        parent: Span,
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
        if !layer.effects.is_empty() {
            return unsupported("effects");
        }
        if layer.mask.is_some() {
            return unsupported("masks");
        }

        let (source, box_w, box_h) = match &layer.content {
            Content::Solid {
                color,
                width,
                height,
            } => {
                // A solid's source image is whole pixels, scaled to its exact box.
                let fill = premultiply(straight(*color));
                let (w, h) = (width.ceil() as u32, height.ceil() as u32);
                let pixmap = Pixmap::filled(w.max(1), h.max(1), fill, self.limits.max_pixels)
                    .map_err(|e| RenderError::TooLarge {
                        what: format!("{path} (solid)"),
                        pixels: e.pixels,
                        limit: e.limit,
                    })?;
                (pixmap, *width, *height)
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
                (image.clone(), bw, bh)
            }
            Content::Group { .. } => return unsupported("groups"),
            Content::Text(_) => return unsupported("text layers"),
            Content::Video { .. } => return unsupported("video layers"),
        };

        let t = span.local_ms(self.moment);
        let moved = transitions(
            layer.transition_in.as_ref(),
            layer.transition_out.as_ref(),
            t,
            span.length_ms(),
            self.width,
            self.height,
        );
        let tf = &layer.transform;
        let x = value_at(&tf.x, t) + moved.dx;
        let y = value_at(&tf.y, t) + moved.dy;
        let scale_x = value_at(&tf.scale_x, t) * moved.scale;
        let scale_y = value_at(&tf.scale_y, t) * moved.scale;
        let anchor_x = value_at(&tf.anchor_x, t);
        let anchor_y = value_at(&tf.anchor_y, t);
        let rotation = value_at(&tf.rotation_deg, t);
        let opacity = (value_at(&layer.opacity, t) * moved.opacity).clamp(0.0, 1.0);

        // Source image to layer box, then scale, rotate, and place the anchor at x, y.
        let to_canvas = Affine::translate(x, y)
            .then_after(Affine::rotate_deg(rotation))
            .then_after(Affine::scale(scale_x, scale_y))
            .then_after(Affine::translate(-anchor_x * box_w, -anchor_y * box_h))
            .then_after(Affine::scale(
                box_w / f64::from(source.width),
                box_h / f64::from(source.height),
            ));
        place(canvas, &source, to_canvas, opacity as f32, layer.blend);
        Ok(())
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
