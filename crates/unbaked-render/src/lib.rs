//! Reference renderer for Unbaked media files (SPEC.md sections 5 and 6).

use std::fmt;

use unbaked_core::json::Problem;
use unbaked_core::package::{Limits, PackageError};
use unbaked_core::recipe::{self, OutputKind};
use unbaked_core::{bake, pack, rules, sha256_hex, sniff};

pub mod draw;
pub mod image;
pub mod motion;
pub mod scene;
pub mod timing;

pub use scene::RenderLimits;

/// Names this renderer in `bake.json`.
pub const RENDERER: &str = concat!("unbaked-render ", env!("CARGO_PKG_VERSION"));

/// Why a render failed.
#[derive(Debug, Clone, PartialEq)]
pub enum RenderError {
    /// `recipe.json` is missing or breaks the spec. Every problem is listed.
    Recipe(Vec<Problem>),
    /// The recipe uses something this renderer does not draw yet.
    Unsupported(String),
    /// An asset could not be decoded.
    Decode {
        asset: String,
        message: String,
    },
    /// A buffer would pass [`RenderLimits::max_pixels`].
    TooLarge {
        what: String,
        pixels: u64,
        limit: u64,
    },
    /// The result could not be packaged.
    Package(PackageError),
    Encode(String),
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderError::Recipe(problems) => {
                write!(f, "recipe.json is invalid:")?;
                for p in problems {
                    write!(f, "\n  {p}")?;
                }
                Ok(())
            }
            RenderError::Unsupported(what) => write!(f, "{what}"),
            RenderError::Decode { asset, message } => write!(f, "{asset}: {message}"),
            RenderError::TooLarge {
                what,
                pixels,
                limit,
            } => write!(
                f,
                "{what} needs {pixels} pixels, more than the limit of {limit}"
            ),
            RenderError::Package(e) => write!(f, "{e}"),
            RenderError::Encode(e) => write!(f, "could not encode the render: {e}"),
        }
    }
}

impl std::error::Error for RenderError {}

/// Renders a package's recipe and returns a complete Unbaked file: the render
/// as the carrier, holding the package with a fresh `bake.json`. Any
/// `bake.json` in `files` is replaced.
pub fn render(files: &pack::Files, limits: RenderLimits) -> Result<Vec<u8>, RenderError> {
    let recipe_json = files.get("recipe.json").ok_or_else(|| {
        RenderError::Recipe(vec![Problem {
            path: String::new(),
            message: "the package has no recipe.json".into(),
        }])
    })?;
    let recipe = recipe::parse(recipe_json).map_err(RenderError::Recipe)?;
    let kind_of = |path: &str| {
        files
            .get(path)
            .map(|d| sniff::detect(&d[..d.len().min(sniff::HEADER_LEN)]))
    };
    let problems = rules::check(&recipe, &kind_of);
    if !problems.is_empty() {
        return Err(RenderError::Recipe(problems));
    }
    if recipe.output.kind != OutputKind::Image {
        return Err(RenderError::Unsupported(
            "audio and video output are not rendered yet".into(),
        ));
    }

    let file = |path: &str| files.get(path).map(Vec::as_slice);
    let pixels = scene::render_still(&recipe, &file, limits)?;
    let png = image::encode_png(pixels.width, pixels.height, &pixels.to_rgba8())
        .map_err(RenderError::Encode)?;

    let assets: serde_json::Map<String, serde_json::Value> = bake::referenced_files(&recipe)
        .into_iter()
        .map(|path| (path.to_owned(), sha256_hex(&files[path]).into()))
        .collect();
    let bake_json = serde_json::json!({
        "unbaked": unbaked_core::SPEC_VERSION,
        "renderer": RENDERER,
        "recipe_sha256": sha256_hex(recipe_json),
        "assets_sha256": assets,
        "render_sha256": sha256_hex(&png),
    });
    let mut packed = files.clone();
    let mut bake_bytes = serde_json::to_vec_pretty(&bake_json).expect("JSON values serialise");
    bake_bytes.push(b'\n');
    packed.insert("bake.json".into(), bake_bytes);
    let package = pack::write(&packed, Limits::default()).map_err(RenderError::Package)?;
    pack::with_package(&png, &package).map_err(|e| RenderError::Encode(e.to_string()))
}
