//! Runs the sample recipes in `tests/` through the Rust reader, so the reader
//! and `schema/recipe.schema.json` cannot drift apart unnoticed.

use std::fs;
use std::path::{Path, PathBuf};

use unbaked_core::json::Problem;
use unbaked_core::recipe;
use unbaked_core::rules;
use unbaked_core::sniff::AssetKind;

fn samples(dir: &str) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests")
        .join(dir);
    let mut files: Vec<_> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no samples in {}", dir.display());
    files
}

fn name(path: &Path) -> String {
    path.file_stem().unwrap().to_string_lossy().into_owned()
}

/// The package the samples assume (tests/recipe/README.md).
fn sample_package(path: &str) -> Option<Option<AssetKind>> {
    if path.contains("missing") {
        return None;
    }
    let ext = path.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    Some(match ext {
        "png" => Some(AssetKind::Png),
        "jpg" => Some(AssetKind::Jpeg),
        "webp" => Some(AssetKind::Webp),
        "mp4" | "m4a" => Some(AssetKind::Mp4),
        "wav" => Some(AssetKind::Wav),
        "flac" => Some(AssetKind::Flac),
        "mp3" => Some(AssetKind::Mp3),
        "ttf" | "otf" => Some(AssetKind::Font),
        _ => None,
    })
}

/// Parses and checks a recipe the way a reader must.
fn problems(bytes: &[u8]) -> Vec<Problem> {
    match recipe::parse(bytes) {
        Ok(r) => rules::check(&r, &sample_package),
        Err(problems) => problems,
    }
}

fn paths(problems: &[Problem]) -> Vec<&str> {
    problems.iter().map(|p| p.path.as_str()).collect()
}

#[test]
fn valid_schema_samples_pass_every_check() {
    for file in samples("schema/valid") {
        let found = problems(&fs::read(&file).unwrap());
        assert!(found.is_empty(), "{}: {found:?}", name(&file));
    }
}

#[test]
fn invalid_schema_samples_fail_where_intended() {
    let expected = [
        ("audio-missing-duration", "/output"),
        ("bad-color", "/layers/0/color"),
        ("bad-id", "/layers/0/id"),
        ("bezier-x-out-of-range", "/layers/0/opacity/keys/0/ease/0"),
        ("blur-extra-param", "/layers/0/effects/0/dx"),
        ("empty-keys", "/layers/0/opacity/keys"),
        ("field-from-other-type", "/layers/0/font"),
        ("fractional-ms", "/layers/0/end_ms"),
        ("future-version", "/unbaked"),
        ("hold-transition", "/layers/0/in/ease"),
        ("ignored-field-still-checked", "/output/fps"),
        ("image-missing-layers", ""),
        ("image-missing-width", "/output"),
        ("keyframe-on-color", "/layers/0/color"),
        ("missing-layers", ""),
        ("negative-at-ms", "/output/at_ms"),
        ("odd-video-size", "/output/width"),
        ("ofl-without-text", "/assets/f/license"),
        ("path-and-ref", "/assets/f"),
        ("path-dot-dot", "/assets/f/path"),
        ("path-outside-assets", "/assets/f/path"),
        ("text-missing-font", "/layers/0"),
        ("time-as-string", "/layers/0/start_ms"),
        ("unknown-effect", "/layers/0/effects/0/type"),
        ("unknown-field", "/layers/0/colour"),
        ("video-missing-fps", "/output"),
    ];
    let files = samples("schema/invalid");
    assert_eq!(
        files.iter().map(|f| name(f)).collect::<Vec<_>>(),
        expected
            .iter()
            .map(|(n, _)| n.to_string())
            .collect::<Vec<_>>(),
        "every sample needs an expected path"
    );
    for (file, (sample, path)) in files.iter().zip(expected) {
        let found = recipe::parse(&fs::read(file).unwrap())
            .err()
            .unwrap_or_else(|| panic!("{sample}: structure should be rejected"));
        assert_eq!(paths(&found), [path], "{sample}: {found:?}");
    }
}

#[test]
fn rule_samples_fail_where_intended() {
    for file in samples("recipe/invalid") {
        let bytes = fs::read(&file).unwrap();
        let expect: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let path = expect["x-expect"]
            .as_str()
            .unwrap_or_else(|| panic!("{} needs x-expect", name(&file)));
        let found = problems(&bytes);
        assert_eq!(paths(&found), [path], "{}: {found:?}", name(&file));
    }
}

#[test]
fn groups_nest_at_most_32_deep() {
    let nested = |depth: usize| {
        let mut layer =
            r##"{"id": "leaf", "type": "solid", "color": "#fff", "width": 1, "height": 1}"##
                .replace("#fff", "#ffffff");
        for i in 0..depth {
            layer = format!(r#"{{"id": "g{i}", "type": "group", "layers": [{layer}]}}"#);
        }
        format!(
            r#"{{"unbaked": 0, "output": {{"kind": "image", "width": 1, "height": 1}}, "assets": {{}}, "layers": [{layer}]}}"#
        )
    };
    assert!(problems(nested(32).as_bytes()).is_empty());
    let found = problems(nested(33).as_bytes());
    let deepest = "/layers/0".to_owned() + &"/layers/0".repeat(32);
    assert_eq!(paths(&found), [deepest.as_str()], "{found:?}");
}
