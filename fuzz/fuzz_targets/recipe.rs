//! Reading `recipe.json` and checking the rules the schema cannot express.

#![no_main]

use libfuzzer_sys::fuzz_target;
use unbaked_core::sniff::AssetKind;
use unbaked_core::{bake, recipe, rules};

fuzz_target!(|json: &[u8]| {
    let Ok(recipe) = recipe::parse(json) else {
        return;
    };
    // Pretend the package holds a file of the kind its extension suggests.
    let kind_of = |path: &str| {
        let kind = match path.rsplit('.').next() {
            Some("png") => AssetKind::Png,
            Some("jpg" | "jpeg") => AssetKind::Jpeg,
            Some("mp4" | "m4a") => AssetKind::Mp4,
            Some("mp3") => AssetKind::Mp3,
            Some("wav") => AssetKind::Wav,
            Some("flac") => AssetKind::Flac,
            Some("ttf" | "otf") => AssetKind::Font,
            Some("txt") => return Some(None),
            _ => return None,
        };
        Some(Some(kind))
    };
    let _ = rules::check(&recipe, &kind_of);
    let _ = bake::referenced_files(&recipe);
});
