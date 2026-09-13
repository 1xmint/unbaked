//! Renders small recipes and checks exact output pixels.

use unbaked_core::package::Limits;
use unbaked_core::recipe;
use unbaked_core::{Status, open, pack};
use unbaked_render::image::{Pixmap, decode_png, encode_png};
use unbaked_render::scene::render_still;
use unbaked_render::{RenderError, RenderLimits, render};

fn still(recipe_json: &str, files: &[(&str, &[u8])]) -> Result<Pixmap, RenderError> {
    let recipe = recipe::parse(recipe_json.as_bytes()).expect("test recipe parses");
    let lookup = |path: &str| {
        files
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, data)| *data)
    };
    render_still(&recipe, &lookup, RenderLimits::default())
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
    let recipe = image_recipe(1, 1, "", r#"{"id": "g", "type": "group", "layers": []}"#);
    assert_eq!(
        still(&recipe, &[]).unwrap_err(),
        RenderError::Unsupported("/layers/0: groups are not rendered yet".into())
    );
    let hidden = image_recipe(
        1,
        1,
        "",
        r#"{"id": "g", "type": "group", "layers": [], "hidden": true}"#,
    );
    assert!(
        still(&hidden, &[]).is_ok(),
        "hidden layers are skipped, not refused"
    );
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

    let file = render(&files, RenderLimits::default()).unwrap();
    assert_eq!(
        file,
        render(&files, RenderLimits::default()).unwrap(),
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
        render(&broken, RenderLimits::default()),
        Err(RenderError::Recipe(_))
    ));
}

#[test]
fn oversized_canvases_fail_with_a_clear_limit() {
    let recipe = image_recipe(100, 100, "", "");
    let recipe = recipe::parse(recipe.as_bytes()).unwrap();
    let lookup = |_: &str| None;
    let err = render_still(&recipe, &lookup, RenderLimits { max_pixels: 9_999 }).unwrap_err();
    assert_eq!(
        err.to_string(),
        "the canvas needs 10000 pixels, more than the limit of 9999"
    );
}
