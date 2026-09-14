//! Renders small recipes and checks exact output pixels.

use unbaked_core::package::Limits;
use unbaked_core::recipe;
use unbaked_core::{Status, open, pack};
use unbaked_render::image::{Pixmap, decode_png, encode_png};
use unbaked_render::scene::render_still;
use unbaked_render::{FontSource, NoFonts, RenderError, RenderLimits, render};

fn still(recipe_json: &str, files: &[(&str, &[u8])]) -> Result<Pixmap, RenderError> {
    let recipe = recipe::parse(recipe_json.as_bytes()).expect("test recipe parses");
    let lookup = |path: &str| {
        files
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, data)| *data)
    };
    render_still(&recipe, &lookup, &NoFonts, RenderLimits::default())
}

fn image_recipe(width: u32, height: u32, extra_output: &str, layers: &str) -> String {
    format!(
        r#"{{"unbaked": 0, "output": {{"kind": "image", "width": {width}, "height": {height}{extra_output}}},
            "assets": {{"pic": {{"path": "assets/pic.png"}}}}, "layers": [{layers}]}}"#
    )
}

/// The 8-bit RGBA value at (x, y).
fn at(image: &Pixmap, x: u32, y: u32) -> [u8; 4] {
    let i = (y * image.width + x) as usize * 4;
    image.to_rgba8()[i..i + 4].try_into().unwrap()
}

const CLEAR: [u8; 4] = [0, 0, 0, 0];

#[test]
fn background_and_an_aligned_solid() {
    let recipe = image_recipe(
        4,
        4,
        r##", "background": "#00ff00ff""##,
        r##"{"id": "box", "type": "solid", "color": "#ff000080", "width": 2, "height": 2,
            "transform": {"x": 1, "y": 1}}"##,
    );
    let out = still(&recipe, &[]).unwrap();
    assert_eq!(at(&out, 0, 0), [0, 255, 0, 255]);
    // Half-transparent red over green.
    assert_eq!(at(&out, 1, 1), [128, 127, 0, 255]);
    assert_eq!(at(&out, 2, 2), [128, 127, 0, 255]);
    assert_eq!(at(&out, 3, 3), [0, 255, 0, 255]);
}

#[test]
fn anchor_scale_and_rotation() {
    // A 2x1 bar anchored at its top-left, placed at (2, 0), turned 90° clockwise:
    // it now covers column 1, rows 0 and 1.
    let recipe = image_recipe(
        4,
        4,
        "",
        r##"{"id": "bar", "type": "solid", "color": "#0000ffff", "width": 2, "height": 1,
            "transform": {"x": 2, "y": 0, "rotation_deg": 90}}"##,
    );
    let out = still(&recipe, &[]).unwrap();
    for y in 0..4 {
        for x in 0..4 {
            let expected = if x == 1 && y < 2 {
                [0, 0, 255, 255]
            } else {
                CLEAR
            };
            assert_eq!(at(&out, x, y), expected, "({x}, {y})");
        }
    }

    // Centre anchor, scaled 2x: a 1x1 solid at (2, 2) is centred on the canvas.
    // Bilinear sampling with transparency outside the source (section 5.3) makes
    // an upscaled single pixel soft: 0.75² = 0.5625 alpha on the four middle
    // pixels, 0.25² on the corners.
    let recipe = image_recipe(
        4,
        4,
        "",
        r##"{"id": "dot", "type": "solid", "color": "#ffffffff", "width": 1, "height": 1,
            "transform": {"x": 2, "y": 2, "anchor_x": 0.5, "anchor_y": 0.5, "scale_x": 2, "scale_y": 2}}"##,
    );
    let out = still(&recipe, &[]).unwrap();
    for (x, y) in [(1, 1), (2, 1), (1, 2), (2, 2)] {
        assert_eq!(at(&out, x, y)[3], 143, "({x}, {y})");
    }
    for (x, y) in [(0, 0), (3, 0), (0, 3), (3, 3)] {
        assert_eq!(at(&out, x, y)[3], 16, "({x}, {y})");
    }
}

#[test]
fn blend_modes_and_opacity() {
    let recipe = image_recipe(
        1,
        1,
        r##", "background": "#808080ff""##,
        r##"{"id": "red", "type": "solid", "color": "#ff0000ff", "width": 1, "height": 1, "blend": "multiply"}"##,
    );
    assert_eq!(at(&still(&recipe, &[]).unwrap(), 0, 0), [128, 0, 0, 255]);

    let recipe = image_recipe(
        1,
        1,
        "",
        r##"{"id": "w", "type": "solid", "color": "#ffffffff", "width": 1, "height": 1, "opacity": 0.5}"##,
    );
    assert_eq!(
        at(&still(&recipe, &[]).unwrap(), 0, 0),
        [255, 255, 255, 128]
    );
}

#[test]
fn timing_keyframes_and_transitions_use_at_ms() {
    let layers = r##"
        {"id": "later", "type": "solid", "color": "#ff0000ff", "width": 4, "height": 1, "start_ms": 1000},
        {"id": "moving", "type": "solid", "color": "#00ff00ff", "width": 1, "height": 1,
         "transform": {"x": {"keys": [{"t_ms": 0, "v": 0}, {"t_ms": 300, "v": 3}]}}},
        {"id": "fading", "type": "solid", "color": "#0000ffff", "width": 4, "height": 1,
         "transform": {"y": 1}, "in": {"type": "fade", "duration_ms": 400}}"##;

    let out = still(&image_recipe(4, 2, r#", "at_ms": 200"#, layers), &[]).unwrap();
    // Not started yet.
    assert_eq!(at(&out, 3, 0), CLEAR);
    // Two thirds of the way from x = 0 to x = 3.
    assert_eq!(at(&out, 2, 0), [0, 255, 0, 255]);
    assert_eq!(at(&out, 0, 0), CLEAR);
    // Half way through the fade.
    assert_eq!(at(&out, 0, 1), [0, 0, 255, 128]);

    let out = still(&image_recipe(4, 2, r#", "at_ms": 1000"#, layers), &[]).unwrap();
    assert_eq!(at(&out, 0, 0), [255, 0, 0, 255]);
    assert_eq!(at(&out, 3, 0), [0, 255, 0, 255]);
    assert_eq!(at(&out, 0, 1), [0, 0, 255, 255]);
}

#[test]
fn image_layers_scale_by_aspect_ratio_and_fade_at_their_edges() {
    // A 2x2 opaque yellow image drawn 4 wide: height follows at 4.
    let pic = encode_png(2, 2, &[255, 255, 0, 255].repeat(4)).unwrap();
    let recipe = image_recipe(
        4,
        4,
        "",
        r#"{"id": "p", "type": "image", "asset": "pic", "width": 4}"#,
    );
    let out = still(&recipe, &[("assets/pic.png", &pic)]).unwrap();
    assert_eq!(at(&out, 1, 1), [255, 255, 0, 255]);
    assert_eq!(at(&out, 2, 2), [255, 255, 0, 255]);
    // Section 5.3: outside the image is transparent, so edge pixels blend towards it.
    assert_eq!(at(&out, 0, 1)[3], 191);
    assert_eq!(at(&out, 0, 0)[3], 143);
}

#[test]
fn unsupported_features_are_refused_clearly() {
    let video = r#"{"id": "v", "type": "video", "asset": "clip"}"#;
    let recipe = image_recipe(1, 1, "", video);
    assert_eq!(
        still(&recipe, &[]).unwrap_err(),
        RenderError::Unsupported("/layers/0: video layers are not rendered yet".into())
    );
    let nested = image_recipe(
        1,
        1,
        "",
        &format!(r#"{{"id": "g", "type": "group", "layers": [{video}]}}"#),
    );
    assert_eq!(
        still(&nested, &[]).unwrap_err(),
        RenderError::Unsupported("/layers/0/layers/0: video layers are not rendered yet".into())
    );
    let hidden = image_recipe(
        1,
        1,
        "",
        &video.replace("\"asset\"", "\"hidden\": true, \"asset\""),
    );
    assert!(
        still(&hidden, &[]).is_ok(),
        "hidden layers are skipped, not refused"
    );
}

/// A 1-high solid: `{"id": .., "type": "solid", ..}` with extra fields.
fn solid(id: &str, color: &str, width: u32, extra: &str) -> String {
    format!(
        r#"{{"id": "{id}", "type": "solid", "color": "{color}", "width": {width}, "height": 1{extra}}}"#
    )
}

#[test]
fn groups_are_isolated_and_move_their_children() {
    // Two overlapping opaque children at half group opacity: the overlap is 50%, not 75%.
    let layers = format!(
        r#"{{"id": "g", "type": "group", "opacity": 0.5, "transform": {{"x": 1}}, "layers": [{}, {}]}}"#,
        solid("a", "#ff0000ff", 2, ""),
        solid("b", "#0000ffff", 2, r#", "transform": {"x": 1}"#),
    );
    let out = still(&image_recipe(4, 1, "", &layers), &[]).unwrap();
    assert_eq!(at(&out, 0, 0), CLEAR);
    assert_eq!(at(&out, 1, 0), [255, 0, 0, 128]);
    assert_eq!(
        at(&out, 2, 0),
        [0, 0, 255, 128],
        "blue covers red inside the group"
    );
    assert_eq!(at(&out, 3, 0), [0, 0, 255, 128]);

    // A multiply child blends only with its group, not with the background below it.
    let layers = format!(
        r#"{{"id": "g", "type": "group", "layers": [{}]}}"#,
        solid("m", "#ff0000ff", 1, r#", "blend": "multiply""#),
    );
    let out = still(
        &image_recipe(1, 1, r##", "background": "#808080ff""##, &layers),
        &[],
    )
    .unwrap();
    assert_eq!(at(&out, 0, 0), [255, 0, 0, 255]);

    // Children's times are measured from the group's start.
    let layers = format!(
        r#"{{"id": "g", "type": "group", "start_ms": 500, "layers": [{}]}}"#,
        solid("late", "#ffffffff", 1, r#", "start_ms": 600"#),
    );
    let before = still(&image_recipe(1, 1, r#", "at_ms": 1000"#, &layers), &[]).unwrap();
    assert_eq!(at(&before, 0, 0), CLEAR);
    let after = still(&image_recipe(1, 1, r#", "at_ms": 1100"#, &layers), &[]).unwrap();
    assert_eq!(at(&after, 0, 0), [255, 255, 255, 255]);
}

#[test]
fn masks_limit_where_a_layer_shows() {
    let masked = |mask: &str, transform: &str| {
        image_recipe(
            4,
            1,
            "",
            &solid(
                "w",
                "#ffffffff",
                4,
                &format!(r#"{transform}, "mask": {mask}"#),
            ),
        )
    };
    let spot = solid("m", "#ffffffff", 2, "");

    let out = still(&masked(&format!(r#"{{"layers": [{spot}]}}"#), ""), &[]).unwrap();
    let row = |out: &Pixmap| (0..4).map(|x| at(out, x, 0)[3]).collect::<Vec<_>>();
    assert_eq!(row(&out), [255, 255, 0, 0]);

    let out = still(
        &masked(&format!(r#"{{"layers": [{spot}], "invert": true}}"#), ""),
        &[],
    )
    .unwrap();
    assert_eq!(row(&out), [0, 0, 255, 255]);

    // Luminance of a half-transparent white mask is 0.5; of black, 0.
    let grey = solid("m", "#ffffff80", 4, "");
    let out = still(
        &masked(
            &format!(r#"{{"layers": [{grey}], "mode": "luminance"}}"#),
            "",
        ),
        &[],
    )
    .unwrap();
    assert_eq!(row(&out), [128, 128, 128, 128]);
    let black = solid("m", "#000000ff", 4, "");
    let out = still(
        &masked(
            &format!(r#"{{"layers": [{black}], "mode": "luminance"}}"#),
            "",
        ),
        &[],
    )
    .unwrap();
    assert_eq!(row(&out), [0, 0, 0, 0]);

    // The mask ignores the masked layer's own transform: moving the layer does not move the mask.
    let out = still(
        &masked(
            &format!(r#"{{"layers": [{spot}]}}"#),
            r#", "transform": {"x": 1}"#,
        ),
        &[],
    )
    .unwrap();
    assert_eq!(row(&out), [0, 255, 0, 0]);
}

#[test]
fn effects_change_the_layer_before_it_is_placed() {
    // A single white pixel blurred with sigma 1 keeps its centre and spreads 3 pixels.
    let blurred = image_recipe(
        7,
        1,
        "",
        &solid(
            "dot",
            "#ffffffff",
            1,
            r#", "transform": {"x": 3}, "effects": [{"type": "blur", "sigma": 1}]"#,
        ),
    );
    let out = still(&blurred, &[]).unwrap();
    let alphas: Vec<f32> = (0..7).map(|x| out.pixel(x, 0)[3]).collect();
    assert!(
        (alphas.iter().sum::<f32>() - 0.39905).abs() < 1e-3,
        "one row of the 2D kernel: {alphas:?}"
    );
    assert!(
        alphas[3] > alphas[2] && alphas[2] > alphas[1] && alphas[1] > alphas[0] && alphas[0] > 0.0
    );
    assert!((alphas[2] - alphas[4]).abs() < 1e-6, "stays centred");

    // A hard shadow two pixels to the right.
    let shadowed = image_recipe(
        4,
        1,
        "",
        &solid(
            "s",
            "#ffffffff",
            1,
            r##", "effects": [{"type": "shadow", "dx": 2, "color": "#000000ff"}]"##,
        ),
    );
    let out = still(&shadowed, &[]).unwrap();
    assert_eq!(at(&out, 0, 0), [255, 255, 255, 255]);
    assert_eq!(at(&out, 1, 0), CLEAR);
    assert_eq!(at(&out, 2, 0), [0, 0, 0, 255]);

    // Saturation 0 on red gives the SVG saturate() grey.
    let adjusted = image_recipe(
        1,
        1,
        "",
        &solid(
            "r",
            "#ff0000ff",
            1,
            r#", "effects": [{"type": "adjust", "saturation": 0}]"#,
        ),
    );
    assert_eq!(at(&still(&adjusted, &[]).unwrap(), 0, 0), [54, 54, 54, 255]);

    // Group effects work in canvas space: brightness 0 turns the whole group black.
    let group = format!(
        r#"{{"id": "g", "type": "group", "effects": [{{"type": "adjust", "brightness": 0}}], "layers": [{}]}}"#,
        solid("w", "#ffffffff", 2, r#", "transform": {"x": 1}"#),
    );
    let out = still(&image_recipe(4, 1, "", &group), &[]).unwrap();
    assert_eq!(at(&out, 0, 0), CLEAR);
    assert_eq!(at(&out, 1, 0), [0, 0, 0, 255]);
}

#[test]
fn rendering_a_package_makes_a_fresh_file() {
    let recipe = image_recipe(
        3,
        2,
        r##", "background": "#102030ff""##,
        r#"{"id": "p", "type": "image", "asset": "pic", "transform": {"x": 1}}"#,
    );
    let pic = encode_png(1, 1, &[200, 100, 50, 255]).unwrap();
    let files: pack::Files = [
        ("recipe.json", recipe.clone().into_bytes()),
        ("assets/pic.png", pic),
    ]
    .into_iter()
    .map(|(n, d)| (n.to_owned(), d))
    .collect();

    let file = render(&files, &NoFonts, RenderLimits::default()).unwrap();
    assert_eq!(
        file,
        render(&files, &NoFonts, RenderLimits::default()).unwrap(),
        "renders repeat exactly"
    );
    let opened = open(&file, Limits::default()).unwrap();
    assert_eq!(opened.check(), Status::Fresh);

    let shown = decode_png(&file, 100).unwrap();
    assert_eq!(at(&shown, 0, 0), [16, 32, 48, 255]);
    assert_eq!(at(&shown, 1, 0), [200, 100, 50, 255]);

    let bad = recipe.replace("\"asset\": \"pic\"", "\"asset\": \"nope\"");
    let mut broken = files.clone();
    broken.insert("recipe.json".into(), bad.into_bytes());
    assert!(matches!(
        render(&broken, &NoFonts, RenderLimits::default()),
        Err(RenderError::Recipe(_))
    ));
}

#[test]
fn oversized_canvases_fail_with_a_clear_limit() {
    let recipe = image_recipe(100, 100, "", "");
    let recipe = recipe::parse(recipe.as_bytes()).unwrap();
    let lookup = |_: &str| None;
    let err = render_still(
        &recipe,
        &lookup,
        &NoFonts,
        RenderLimits { max_pixels: 9_999 },
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "the canvas needs 10000 pixels, more than the limit of 9999"
    );
}

const LATO: &[u8] = include_bytes!("../../../tests/fonts/Lato-Regular.ttf");
const OFL: &[u8] = include_bytes!("../../../tests/fonts/OFL.txt");

/// Hands out Lato for its own SHA-256, or other bytes claiming to be it.
struct Fonts(&'static [u8]);

impl FontSource for Fonts {
    fn find(&self, _sha256: &str) -> Option<Vec<u8>> {
        Some(self.0.to_vec())
    }
}

fn text_recipe(font_asset: &str) -> String {
    format!(
        r##"{{"unbaked": 0, "output": {{"kind": "image", "width": 60, "height": 40, "background": "#ffffffff"}},
            "assets": {{"lato": {font_asset}}},
            "layers": [{{"id": "t", "type": "text", "text": "Hi", "font": "lato", "size_px": 30,
                "color": "#000000ff", "transform": {{"x": 10, "y": 2}}}}]}}"##
    )
}

const PACKED: &str = r#"{"path": "assets/fonts/Lato-Regular.ttf", "license": {"spdx": "OFL-1.1", "file": "assets/fonts/OFL.txt"}}"#;

fn dark_pixels(image: &Pixmap) -> Vec<(u32, u32)> {
    let mut dark = Vec::new();
    for y in 0..image.height {
        for x in 0..image.width {
            if at(image, x, y)[0] < 128 {
                dark.push((x, y));
            }
        }
    }
    dark
}

#[test]
fn text_layers_render_with_a_packed_font() {
    let files: pack::Files = [
        ("recipe.json", text_recipe(PACKED).into_bytes()),
        ("assets/fonts/Lato-Regular.ttf", LATO.to_vec()),
        ("assets/fonts/OFL.txt", OFL.to_vec()),
    ]
    .into_iter()
    .map(|(n, d)| (n.to_owned(), d))
    .collect();
    let file = render(&files, &NoFonts, RenderLimits::default()).unwrap();
    assert_eq!(
        file,
        render(&files, &NoFonts, RenderLimits::default()).unwrap()
    );
    assert_eq!(
        open(&file, Limits::default()).unwrap().check(),
        Status::Fresh
    );

    let shown = decode_png(&file, 10_000).unwrap();
    let dark = dark_pixels(&shown);
    assert!(dark.len() > 50, "{} dark pixels", dark.len());
    // "H" has a left side bearing, so no ink reaches left of the box at x = 10.
    let (min_x, max_x) = (
        dark.iter().map(|p| p.0).min().unwrap(),
        dark.iter().map(|p| p.0).max().unwrap(),
    );
    let (min_y, max_y) = (
        dark.iter().map(|p| p.1).min().unwrap(),
        dark.iter().map(|p| p.1).max().unwrap(),
    );
    assert!((11..16).contains(&min_x), "{min_x}");
    assert!(max_x < 10 + 30, "{max_x}");
    // Cap height sits below the box top at y = 2; the baseline is above the box bottom at 2 + 36.
    assert!(min_y > 2 && max_y < 38, "{min_y}..{max_y}");
}

#[test]
fn referenced_fonts_are_found_by_fingerprint_only() {
    let sha = unbaked_core::sha256_hex(LATO);
    let reference =
        format!(r#"{{"ref": {{"family": "Lato", "style": "Regular", "sha256": "{sha}"}}}}"#);
    let recipe = recipe::parse(text_recipe(&reference).as_bytes()).unwrap();
    let no_files = |_: &str| None;

    let err = render_still(&recipe, &no_files, &NoFonts, RenderLimits::default()).unwrap_err();
    assert_eq!(
        err.to_string(),
        "font \"Lato\" (Regular) was not found: no font file given has its SHA-256"
    );
    // Bytes whose hash differs are refused, even when a source offers them.
    let wrong = render_still(&recipe, &no_files, &Fonts(OFL), RenderLimits::default());
    assert!(matches!(wrong, Err(RenderError::FontNotFound { .. })));

    let referenced =
        render_still(&recipe, &no_files, &Fonts(LATO), RenderLimits::default()).unwrap();
    let packed = still(
        &text_recipe(PACKED),
        &[("assets/fonts/Lato-Regular.ttf", LATO)],
    )
    .unwrap();
    assert_eq!(referenced, packed);
}

#[test]
fn text_moves_with_its_transform_and_takes_effects() {
    let recipe = text_recipe(PACKED);
    let files = [("assets/fonts/Lato-Regular.ttf", LATO)];
    let base = dark_pixels(&still(&recipe, &files).unwrap());
    let moved = recipe.replace(r#""x": 10, "y": 2"#, r#""x": 15, "y": 2"#);
    let shifted = dark_pixels(&still(&moved, &files).unwrap());
    assert_eq!(
        shifted,
        base.iter().map(|&(x, y)| (x + 5, y)).collect::<Vec<_>>()
    );

    // A shadow draws under the text, offset down and right.
    let shadowed = recipe.replace(
        r##""color": "#000000ff","##,
        r##""color": "#000000ff", "effects": [{"type": "shadow", "dx": 20, "dy": 0, "color": "#ff0000ff"}],"##,
    );
    let out = still(&shadowed, &files).unwrap();
    let red = (0..out.width)
        .flat_map(|x| (0..out.height).map(move |y| (x, y)))
        .filter(|&(x, y)| at(&out, x, y) == [255, 0, 0, 255])
        .count();
    assert!(red > 50, "{red} red pixels");
}
