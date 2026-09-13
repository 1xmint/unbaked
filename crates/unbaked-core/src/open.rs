//! Opening a whole Unbaked file: find the carrier, read the package, check the
//! recipe, and compare the fingerprints.

use std::collections::BTreeMap;
use std::fmt;

use sha2::{Digest, Sha256};

use crate::bake::{self, Current, Freshness};
use crate::json::Problem;
use crate::mp4::{self, Mp4Error};
use crate::package::{Limits, Package, PackageError};
use crate::png::{self, PngError};
use crate::recipe::{self, AssetSource, OutputKind, Recipe};
use crate::rules;
use crate::sniff::{self, AssetKind};

/// The container a file's first bytes show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Png,
    /// MP4 or M4A: both are ISO BMFF files starting with an `ftyp` box.
    Mp4,
}

/// The container of a file, from its first bytes.
pub fn detect(file: &[u8]) -> Option<Container> {
    if file.starts_with(&png::SIGNATURE) {
        Some(Container::Png)
    } else if file.get(4..8) == Some(b"ftyp") {
        Some(Container::Mp4)
    } else {
        None
    }
}

/// Why a file could not be opened as an Unbaked file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// Neither a PNG nor an MP4.
    UnknownFormat,
    /// A normal PNG or MP4 with no hidden package.
    NoPackage,
    Png(PngError),
    Mp4(Mp4Error),
    Package(PackageError),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpenError::UnknownFormat => write!(f, "not a PNG or MP4 file"),
            OpenError::NoPackage => write!(f, "not an Unbaked file: it has no hidden package"),
            OpenError::Png(e) => write!(f, "PNG: {e}"),
            OpenError::Mp4(e) => write!(f, "MP4: {e}"),
            OpenError::Package(e) => write!(f, "package: {e}"),
        }
    }
}

impl std::error::Error for OpenError {}

#[derive(Debug, Clone)]
struct FileInfo {
    sha256: String,
    kind: Option<AssetKind>,
}

/// A file whose carrier and package passed every check. Its recipe and
/// fingerprints are checked by [`Opened::check`].
#[derive(Debug, Clone)]
pub struct Opened {
    container: Container,
    recipe_json: Vec<u8>,
    bake_json: Vec<u8>,
    files: BTreeMap<String, FileInfo>,
    render_sha256: String,
}

/// The result of checking an opened file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// `recipe.json` or `bake.json` breaks the rules. Every problem found is listed.
    Invalid {
        recipe: Vec<Problem>,
        bake: Vec<Problem>,
    },
    Fresh,
    /// Lists what changed since the render.
    Stale(Vec<bake::Change>),
    RenderModified,
}

/// Opens a file: detects the carrier, reads the hidden package, and checks the
/// package against section 3, reading every file so sizes and checksums are verified.
pub fn open(file: &[u8], limits: Limits) -> Result<Opened, OpenError> {
    let container = detect(file).ok_or(OpenError::UnknownFormat)?;
    let (slot, without) = match container {
        Container::Png => (
            png::read_slot(file).map_err(OpenError::Png)?,
            png::without_slot(file).map_err(OpenError::Png)?,
        ),
        Container::Mp4 => (
            mp4::read_slot(file).map_err(OpenError::Mp4)?,
            mp4::without_slot(file).map_err(OpenError::Mp4)?,
        ),
    };
    let slot = slot.ok_or(OpenError::NoPackage)?;
    let mut package = Package::open(slot, limits).map_err(OpenError::Package)?;

    let names: Vec<String> = package.file_names().map(str::to_owned).collect();
    let mut files = BTreeMap::new();
    let mut recipe_json = Vec::new();
    let mut bake_json = Vec::new();
    for name in names {
        let bytes = package.read(&name).map_err(OpenError::Package)?;
        let info = FileInfo {
            sha256: sha256_hex(&bytes),
            kind: sniff::detect(&bytes[..bytes.len().min(sniff::HEADER_LEN)]),
        };
        match name.as_str() {
            "recipe.json" => recipe_json = bytes,
            "bake.json" => bake_json = bytes,
            _ => {}
        }
        files.insert(name, info);
    }

    Ok(Opened {
        container,
        recipe_json,
        bake_json,
        files,
        render_sha256: sha256_hex(&without),
    })
}

/// SHA-256 as 64 lowercase hex characters.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use fmt::Write;
    let mut out = String::with_capacity(64);
    for b in Sha256::digest(bytes).iter() {
        let _ = write!(out, "{b:02x}");
    }
    out
}

impl Opened {
    pub fn container(&self) -> Container {
        self.container
    }

    /// `recipe.json` exactly as stored.
    pub fn recipe_json(&self) -> &[u8] {
        &self.recipe_json
    }

    /// `bake.json` exactly as stored.
    pub fn bake_json(&self) -> &[u8] {
        &self.bake_json
    }

    /// Names of the package's files, as [`Package::file_names`] lists them.
    pub fn file_names(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }

    /// Parses the recipe and checks every rule, including those the schema cannot express.
    pub fn recipe(&self) -> Result<Recipe, Vec<Problem>> {
        let recipe = recipe::parse(&self.recipe_json)?;
        let lookup = |path: &str| self.files.get(path).map(|f| f.kind);
        let problems = rules::check(&recipe, &lookup);
        if problems.is_empty() {
            Ok(recipe)
        } else {
            Err(problems)
        }
    }

    /// Checks the recipe and `bake.json`, then compares fingerprints (section 7).
    pub fn check(&self) -> Status {
        let recipe = self.recipe();
        let bake = bake::parse(&self.bake_json);
        let (recipe, bake) = match (recipe, bake) {
            (Ok(r), Ok(b)) => (r, b),
            (r, b) => {
                return Status::Invalid {
                    recipe: r.err().unwrap_or_default(),
                    bake: b.err().unwrap_or_default(),
                };
            }
        };

        let recipe_sha256 = sha256_hex(&self.recipe_json);
        let mut assets_sha256 = BTreeMap::new();
        for asset in recipe.assets.values() {
            let paths = [
                match &asset.source {
                    AssetSource::Path(path) => Some(path),
                    AssetSource::Ref(_) => None,
                },
                asset.license.as_ref().and_then(|l| l.file.as_ref()),
            ];
            for path in paths.into_iter().flatten() {
                // The rules already confirmed every referenced file exists.
                if let Some(info) = self.files.get(path) {
                    assets_sha256.insert(path.as_str(), info.sha256.as_str());
                }
            }
        }
        let carrier_fits = match recipe.output.kind {
            OutputKind::Image => self.container == Container::Png,
            OutputKind::Audio | OutputKind::Video => self.container == Container::Mp4,
        };
        let now = Current {
            recipe_sha256: &recipe_sha256,
            assets_sha256,
            render_sha256: &self.render_sha256,
            carrier_fits,
        };
        match bake::compare(&bake, &now) {
            Freshness::Fresh => Status::Fresh,
            Freshness::Stale(changes) => Status::Stale(changes),
            Freshness::RenderModified => Status::RenderModified,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bake::Change;
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&png::crc32(&[kind, data]).to_be_bytes());
        out
    }

    /// A valid 1x1 RGBA PNG holding one pixel.
    fn png_pixel(rgba: [u8; 4]) -> Vec<u8> {
        let raw = [0, rgba[0], rgba[1], rgba[2], rgba[3]];
        let (mut a, mut b) = (1u32, 0u32);
        for byte in raw {
            a = (a + u32::from(byte)) % 65521;
            b = (b + a) % 65521;
        }
        let mut idat = vec![0x78, 0x01, 0x01, 0x05, 0x00, 0xfa, 0xff];
        idat.extend_from_slice(&raw);
        idat.extend_from_slice(&((b << 16) | a).to_be_bytes());
        let mut png = png::SIGNATURE.to_vec();
        png.extend(chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]));
        png.extend(chunk(b"IDAT", &idat));
        png.extend(chunk(b"IEND", &[]));
        png
    }

    fn mp4_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = ((8 + payload.len()) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    }

    fn tiny_mp4() -> Vec<u8> {
        [
            mp4_box(b"ftyp", b"isom\0\0\x02\0isomiso2mp41"),
            mp4_box(b"moov", b"placeholder"),
            mp4_box(b"mdat", b"media bytes"),
        ]
        .concat()
    }

    fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in files {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    const IMAGE_RECIPE: &str = r##"{
        "unbaked": 0,
        "output": { "kind": "image", "width": 1, "height": 1 },
        "assets": { "logo": { "path": "assets/logo.png" } },
        "layers": [ { "id": "logo", "type": "image", "asset": "logo" } ]
    }"##;

    const VIDEO_RECIPE: &str = r##"{
        "unbaked": 0,
        "output": { "kind": "video", "width": 2, "height": 2, "fps": "30", "duration_ms": 1000 },
        "assets": {},
        "layers": [ { "id": "bg", "type": "solid", "color": "#336699", "width": 2, "height": 2 } ]
    }"##;

    fn bake_for(recipe: &str, assets: &[(&str, &[u8])], render: &[u8]) -> String {
        let assets: Vec<String> = assets
            .iter()
            .map(|(path, data)| format!("{path:?}: {:?}", sha256_hex(data)))
            .collect();
        format!(
            r#"{{"unbaked": 0, "renderer": "test", "recipe_sha256": "{}",
                "assets_sha256": {{ {} }}, "render_sha256": "{}"}}"#,
            sha256_hex(recipe.as_bytes()),
            assets.join(", "),
            sha256_hex(render)
        )
    }

    /// A carrier holding `recipe`, `assets` and the given `bake.json`.
    fn unbaked_png(render: &[u8], recipe: &str, assets: &[(&str, &[u8])], bake: &str) -> Vec<u8> {
        let mut files = vec![
            ("recipe.json", recipe.as_bytes()),
            ("bake.json", bake.as_bytes()),
        ];
        files.extend_from_slice(assets);
        png::write_slot(render, &zip(&files)).unwrap()
    }

    fn status(file: &[u8]) -> Status {
        open(file, Limits::default()).unwrap().check()
    }

    #[test]
    fn png_file_reports_fresh_stale_and_render_modified() {
        let render = png_pixel([10, 20, 30, 255]);
        let logo = png_pixel([1, 2, 3, 4]);
        let assets: &[(&str, &[u8])] = &[("assets/logo.png", &logo)];
        let bake = bake_for(IMAGE_RECIPE, assets, &render);

        let fresh = unbaked_png(&render, IMAGE_RECIPE, assets, &bake);
        let opened = open(&fresh, Limits::default()).unwrap();
        assert_eq!(opened.container(), Container::Png);
        assert_eq!(opened.recipe_json(), IMAGE_RECIPE.as_bytes());
        assert_eq!(opened.check(), Status::Fresh);

        let edited = IMAGE_RECIPE.replace("\"id\": \"logo\"", "\"id\": \"mark\"");
        assert_eq!(
            status(&unbaked_png(&render, &edited, assets, &bake)),
            Status::Stale(vec![Change::Recipe])
        );

        let new_logo = png_pixel([9, 9, 9, 9]);
        assert_eq!(
            status(&unbaked_png(
                &render,
                IMAGE_RECIPE,
                &[("assets/logo.png", &new_logo)],
                &bake
            )),
            Status::Stale(vec![Change::AssetChanged("assets/logo.png".into())])
        );

        let with_licence = IMAGE_RECIPE.replace(
            r#""path": "assets/logo.png" }"#,
            r#""path": "assets/logo.png", "license": { "spdx": "CC0-1.0", "file": "assets/CC0.txt" } }"#,
        );
        assert_eq!(
            status(&unbaked_png(
                &render,
                &with_licence,
                &[("assets/logo.png", &logo), ("assets/CC0.txt", b"text")],
                &bake
            )),
            Status::Stale(vec![
                Change::Recipe,
                Change::AssetAdded("assets/CC0.txt".into())
            ])
        );

        let retouched = png_pixel([11, 20, 30, 255]);
        assert_eq!(
            status(&unbaked_png(&retouched, IMAGE_RECIPE, assets, &bake)),
            Status::RenderModified
        );
    }

    #[test]
    fn problems_in_recipe_and_bake_are_both_reported() {
        let render = png_pixel([0, 0, 0, 255]);
        let bad_recipe = IMAGE_RECIPE.replace("\"asset\": \"logo\"", "\"asset\": \"nope\"");
        let bad_bake = r#"{"unbaked": 0}"#;
        let file = unbaked_png(
            &render,
            &bad_recipe,
            &[("assets/logo.png", &render)],
            bad_bake,
        );
        let Status::Invalid { recipe, bake } = status(&file) else {
            panic!("should be invalid");
        };
        assert_eq!(recipe.len(), 1);
        assert_eq!(recipe[0].path, "/layers/0/asset");
        let missing: Vec<_> = bake.iter().map(|p| p.message.as_str()).collect();
        assert_eq!(
            missing,
            [
                "missing required field \"renderer\"",
                "missing required field \"recipe_sha256\"",
                "missing required field \"assets_sha256\"",
                "missing required field \"render_sha256\"",
            ]
        );
    }

    #[test]
    fn asset_kind_is_read_from_the_package_bytes() {
        let render = png_pixel([0, 0, 0, 255]);
        let not_png = b"just text, named .png";
        let assets: &[(&str, &[u8])] = &[("assets/logo.png", not_png)];
        let bake = bake_for(IMAGE_RECIPE, assets, &render);
        let Status::Invalid { recipe, .. } =
            status(&unbaked_png(&render, IMAGE_RECIPE, assets, &bake))
        else {
            panic!("should be invalid");
        };
        assert_eq!(recipe[0].path, "/layers/0/asset");
    }

    #[test]
    fn mp4_file_is_fresh_and_a_kind_switch_is_stale() {
        let render = tiny_mp4();
        let bake = bake_for(VIDEO_RECIPE, &[], &render);
        let files: &[(&str, &[u8])] = &[
            ("recipe.json", VIDEO_RECIPE.as_bytes()),
            ("bake.json", bake.as_bytes()),
        ];
        let file = mp4::write_slot(&render, &zip(files)).unwrap();
        let opened = open(&file, Limits::default()).unwrap();
        assert_eq!(opened.container(), Container::Mp4);
        assert_eq!(opened.check(), Status::Fresh);

        let still = VIDEO_RECIPE.replace("\"video\"", "\"image\"");
        let files: &[(&str, &[u8])] = &[
            ("recipe.json", still.as_bytes()),
            ("bake.json", bake.as_bytes()),
        ];
        let switched = mp4::write_slot(&render, &zip(files)).unwrap();
        assert_eq!(
            status(&switched),
            Status::Stale(vec![Change::Recipe, Change::Carrier])
        );
    }

    #[test]
    fn files_that_are_not_unbaked_say_why() {
        let plain = png_pixel([0, 0, 0, 255]);
        assert_eq!(
            open(&plain, Limits::default()).err(),
            Some(OpenError::NoPackage)
        );
        assert_eq!(
            open(&tiny_mp4(), Limits::default()).err(),
            Some(OpenError::NoPackage)
        );
        assert_eq!(
            open(b"GIF89a", Limits::default()).err(),
            Some(OpenError::UnknownFormat)
        );
        let broken = png::write_slot(&plain, b"not a zip").unwrap();
        assert!(matches!(
            open(&broken, Limits::default()),
            Err(OpenError::Package(PackageError::Zip(_)))
        ));
        let truncated = &plain[..plain.len() - 3];
        assert_eq!(
            open(truncated, Limits::default()).err(),
            Some(OpenError::Png(PngError::Truncated))
        );
    }
}
