//! Renders small recipes and checks exact output pixels.

use std::time::Instant;

use unbaked_core::package::Limits;
use unbaked_core::recipe;
use unbaked_core::{Status, open, pack};
use unbaked_render::image::{Pixmap, decode_png, encode_png};
use unbaked_render::scene::render_still;
use unbaked_render::{Deadline, FontSource, NoFonts, RenderError, RenderLimits, render};

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

const CLIP: &[u8] = include_bytes!("../../../tests/video/frames-high.mp4");

/// The frame number in the test clip: its top-left block has luma 24 + 16·n.
fn clip_frame(image: &Pixmap, x: u32, y: u32) -> i32 {
    let luma = f64::from(at(image, x, y)[1]) / 255.0 * 219.0 + 16.0;
    ((luma - 24.0) / 16.0).round() as i32
}

fn video_recipe(output: &str, layer: &str) -> String {
    format!(
        r#"{{"unbaked": 0, "output": {output},
            "assets": {{"clip": {{"path": "assets/clip.mp4"}}}},
            "layers": [{{"id": "v", "type": "video", "asset": "clip"{layer}}}]}}"#
    )
}

#[test]
fn video_layers_show_the_frame_at_their_source_time() {
    let files = [("assets/clip.mp4", CLIP)];
    let shown = |at_ms: u64, layer: &str| {
        let output = format!(r#"{{"kind": "image", "width": 96, "height": 64, "at_ms": {at_ms}}}"#);
        still(&video_recipe(&output, layer), &files).unwrap()
    };
    // The clip's frames are 100 ms apart, with frame 0 shown for the first 300 ms.
    assert_eq!(clip_frame(&shown(450, ""), 8, 8), 2);
    assert_eq!(clip_frame(&shown(1150, ""), 8, 8), 9);
    assert_eq!(
        clip_frame(&shown(60_000, ""), 8, 8),
        9,
        "the last frame is held"
    );
    // Trim moves into the clip; start delays it.
    assert_eq!(clip_frame(&shown(50, r#", "trim_start_ms": 700"#), 8, 8), 5);
    assert_eq!(clip_frame(&shown(750, r#", "start_ms": 300"#), 8, 8), 2);
    assert_eq!(at(&shown(250, r#", "start_ms": 300"#), 8, 8), CLEAR);
    // The box follows the frame's aspect ratio.
    let half = shown(450, r#", "width": 48"#);
    assert_eq!(clip_frame(&half, 4, 4), 2);
    assert_eq!(at(&half, 60, 40), CLEAR);
    assert_eq!(at(&half, 47, 31)[3], 255);

    let hidden = video_recipe(
        r#"{"kind": "image", "width": 1, "height": 1}"#,
        r#", "hidden": true"#,
    );
    assert!(
        still(&hidden, &[]).is_ok(),
        "hidden layers are skipped, not opened"
    );
    let broken = [("assets/clip.mp4", &CLIP[..CLIP.len() / 2])];
    let output = r#"{"kind": "image", "width": 96, "height": 64}"#;
    assert!(matches!(
        still(&video_recipe(output, ""), &broken),
        Err(RenderError::Decode { asset, .. }) if asset == "assets/clip.mp4"
    ));
}

#[test]
fn two_layers_can_show_one_clip_at_different_times() {
    use unbaked_render::scene::Frames;
    use unbaked_render::timing::Moment;

    // One decoder serves both layers, so each frame asks it to jump back and forth.
    let recipe = recipe::parse(
        br#"{"unbaked": 0, "output": {"kind": "image", "width": 192, "height": 64},
            "assets": {"clip": {"path": "assets/clip.mp4"}},
            "layers": [{"id": "late", "type": "video", "asset": "clip", "trim_start_ms": 600},
                       {"id": "early", "type": "video", "asset": "clip", "transform": {"x": 96}}]}"#,
    )
    .unwrap();
    let lookup = |path: &str| (path == "assets/clip.mp4").then_some(CLIP);
    let mut frames = Frames::new(&recipe, &lookup, &NoFonts, RenderLimits::default()).unwrap();
    let files = [("assets/clip.mp4", CLIP)];
    let alone = |at_ms: u64, trim: u64| {
        let output = format!(r#"{{"kind": "image", "width": 96, "height": 64, "at_ms": {at_ms}}}"#);
        let layer = format!(r#", "trim_start_ms": {trim}"#);
        clip_frame(
            &still(&video_recipe(&output, &layer), &files).unwrap(),
            8,
            8,
        )
    };
    for at_ms in (0..1300).step_by(50) {
        let canvas = frames.draw(Moment::AtMs(at_ms)).unwrap();
        assert_eq!(
            clip_frame(&canvas, 8, 8),
            alone(at_ms, 600),
            "late at {at_ms}"
        );
        assert_eq!(
            clip_frame(&canvas, 104, 8),
            alone(at_ms, 0),
            "early at {at_ms}"
        );
    }
}

#[test]
fn video_recipes_render_a_fresh_mp4() {
    use unbaked_render::mp4;
    use unbaked_render::sound::decode;
    use unbaked_render::video::Video;
    // One second at 10 fps: red, then the test clip from 700 ms in over the
    // right half from 500 ms, and a beep in the second half.
    let recipe = r##"{"unbaked": 0,
        "output": {"kind": "video", "width": 96, "height": 64, "fps": "10", "duration_ms": 1000,
                   "background": "#cc2020ff"},
        "assets": {"clip": {"path": "assets/clip.mp4"}, "beep": {"path": "assets/beep.wav"}},
        "layers": [{"id": "v", "type": "video", "asset": "clip", "start_ms": 500,
                    "trim_start_ms": 700, "transform": {"x": 48}}],
        "audio": [{"id": "b", "asset": "beep", "start_ms": 500}]}"##;
    let beep: Vec<i16> = tone(1000.0, 48000, 24000, 0.5)
        .iter()
        .map(|s| (s * 32767.0) as i16)
        .collect();
    let mut files: pack::Files = [
        ("recipe.json", recipe.as_bytes().to_vec()),
        ("assets/clip.mp4", CLIP.to_vec()),
        ("assets/beep.wav", wav(48000, 1, &beep)),
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
    assert_eq!(
        open(&file, Limits::default()).unwrap().check(),
        Status::Fresh
    );

    let movie = mp4::read(&file, 10_000).unwrap();
    let handlers: Vec<&[u8; 4]> = movie.tracks.iter().map(|t| &t.handler).collect();
    assert_eq!(handlers, [b"vide", b"soun"]);
    assert_eq!(movie.tracks[0].samples.len(), 10);

    let mut video = Video::open(&file, 10_000, 1_000_000).unwrap();
    let red = [0.8, 0.125, 0.125];
    let close = |got: [f32; 4], want: [f32; 3]| {
        got[..3].iter().zip(want).all(|(g, w)| (g - w).abs() < 0.04)
    };
    for at in [50, 450, 950] {
        let frame = video.frame_at(at, 1).unwrap();
        assert!(close(frame.pixel(20, 30), red), "{at} ms");
    }
    let before = video.frame_at(450, 1).unwrap();
    assert!(close(before.pixel(60, 8), red), "the clip has not started");
    // At 550 ms the clip is 50 ms in, plus the 700 ms trim: its frame 5.
    let during = video.frame_at(550, 1).unwrap();
    let luma = f64::from(during.pixel(52, 8)[1]) * 219.0 + 16.0;
    assert_eq!(
        ((luma - 24.0) / 16.0).round(),
        5.0,
        "{:?}",
        during.pixel(52, 8)
    );

    let sound = decode(&file, 1_000_000).unwrap();
    assert_eq!((sound.rate, sound.len()), (48000, 48000));
    assert!(rms(&sound.channels[0][4_800..19_200]) < 0.01);
    assert!(rms(&sound.channels[0][28_800..43_200]) > 0.3);

    // Without audio clips there is no sound track.
    let silent = recipe.replace(
        r#""audio": [{"id": "b", "asset": "beep", "start_ms": 500}]"#,
        r#""audio": []"#,
    );
    files.insert("recipe.json".into(), silent.into_bytes());
    let file = render(&files, &NoFonts, RenderLimits::default()).unwrap();
    assert_eq!(mp4::read(&file, 10_000).unwrap().tracks.len(), 1);

    let err = render(
        &files,
        &NoFonts,
        RenderLimits {
            max_frames: 9,
            ..RenderLimits::default()
        },
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "the video has 10 frames, more than the limit of 9"
    );
    let huge = String::from_utf8(files["recipe.json"].clone())
        .unwrap()
        .replace(r#""width": 96"#, r#""width": 4000"#);
    files.insert("recipe.json".into(), huge.into_bytes());
    assert_eq!(
        render(&files, &NoFonts, RenderLimits::default()).unwrap_err(),
        RenderError::Unsupported("video larger than 3840×2160 is not rendered yet".into())
    );

    // The same clip in a still renders to a fresh PNG.
    let still_recipe = video_recipe(
        r#"{"kind": "image", "width": 96, "height": 64, "at_ms": 450}"#,
        "",
    );
    files.insert("recipe.json".into(), still_recipe.into_bytes());
    let file = render(&files, &NoFonts, RenderLimits::default()).unwrap();
    assert_eq!(
        open(&file, Limits::default()).unwrap().check(),
        Status::Fresh
    );
    assert_eq!(clip_frame(&decode_png(&file, 10_000).unwrap(), 8, 8), 2);
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
        RenderLimits {
            max_pixels: 9_999,
            ..RenderLimits::default()
        },
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

/// A 16-bit PCM WAV file.
fn wav(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
    out.extend_from_slice(&(channels * 2).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

fn tone(freq: f64, rate: u32, n: usize, amp: f64) -> Vec<f32> {
    (0..n)
        .map(|i| {
            (amp * (2.0 * std::f64::consts::PI * freq * i as f64 / f64::from(rate)).sin()) as f32
        })
        .collect()
}

fn rms(s: &[f32]) -> f32 {
    (s.iter().map(|v| v * v).sum::<f32>() / s.len() as f32).sqrt()
}

#[test]
fn audio_recipes_render_a_fresh_m4a() {
    use unbaked_render::sound::{Pcm, decode, encode_m4a};
    // A mono 1 kHz WAV at 44.1 kHz, and a stereo 300 Hz M4A made by our own encoder.
    let beep: Vec<i16> = tone(1000.0, 44100, 22050, 0.5)
        .iter()
        .map(|s| (s * 32767.0) as i16)
        .collect();
    let hum = encode_m4a(&Pcm {
        rate: 48000,
        channels: vec![
            tone(300.0, 48000, 48000, 0.4),
            tone(300.0, 48000, 48000, 0.4),
        ],
    })
    .unwrap();
    let recipe = r#"{"unbaked": 0, "output": {"kind": "audio", "duration_ms": 1000, "sample_rate": 48000, "channels": 2},
        "assets": {"beep": {"path": "assets/beep.wav"}, "hum": {"path": "assets/hum.m4a"}},
        "audio": [
            {"id": "b", "asset": "beep", "start_ms": 500},
            {"id": "h", "asset": "hum", "duration_ms": 250, "gain_db": -6},
            {"id": "m", "asset": "hum", "muted": true}
        ]}"#;
    let files: pack::Files = [
        ("recipe.json", recipe.as_bytes().to_vec()),
        ("assets/beep.wav", wav(44100, 1, &beep)),
        ("assets/hum.m4a", hum),
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

    let out = decode(&file, 1_000_000).unwrap();
    assert_eq!((out.rate, out.channels.len(), out.len()), (48000, 2, 48000));
    let left = &out.channels[0];
    // 0–250 ms: the hum at -6 dB (0.4 × 0.5 amplitude). 250–500 ms: silence. 500 ms on: the beep.
    let hum_level = rms(&left[2_400..9_600]);
    assert!((hum_level - 0.2 / 2f32.sqrt()).abs() < 0.02, "{hum_level}");
    assert!(rms(&left[13_200..22_800]) < 0.01);
    let beep_level = rms(&left[26_400..45_600]);
    assert!(
        (beep_level - 0.5 / 2f32.sqrt()).abs() < 0.03,
        "{beep_level}"
    );
    assert_eq!(
        out.channels[0], out.channels[1],
        "mono sources copy to both sides"
    );

    // Too long for the limit: a clear error, not an allocation.
    let err = render(
        &files,
        &NoFonts,
        RenderLimits {
            max_samples: 1000,
            ..RenderLimits::default()
        },
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "the sound needs 48000 samples per channel, more than the limit of 1000"
    );
}

#[test]
fn a_huge_blur_stops_at_the_time_limit() {
    // Radius 300 on a 1500x1500 solid: billions of samples, far past 50 ms.
    let recipe_json = image_recipe(
        1500,
        1500,
        "",
        r##"{"id": "wall", "type": "solid", "color": "#ffffffff", "width": 1500, "height": 1500,
            "effects": [{"type": "blur", "sigma": 100}]}"##,
    );
    let recipe = recipe::parse(recipe_json.as_bytes()).unwrap();
    let limits = RenderLimits {
        deadline: Some(Deadline::after_ms(50)),
        ..RenderLimits::default()
    };
    let started = Instant::now();
    let err = render_still(&recipe, &|_| None, &NoFonts, limits).unwrap_err();
    assert_eq!(err, RenderError::TimedOut { limit_ms: 50 });
    assert_eq!(
        err.to_string(),
        "the render took longer than the limit of 50 ms"
    );
    // Allocating the buffers takes a moment in a debug build; the blur itself does not run on.
    assert!(started.elapsed().as_secs() < 5, "{:?}", started.elapsed());
}

#[test]
fn a_passed_deadline_stops_sound_and_video() {
    let limits = RenderLimits {
        deadline: Some(Deadline::after_ms(0)),
        ..RenderLimits::default()
    };
    let audio = pack::Files::from([(
        "recipe.json".to_owned(),
        br#"{"unbaked": 0, "output": {"kind": "audio", "duration_ms": 100}, "assets": {}, "audio": []}"#.to_vec(),
    )]);
    let video = pack::Files::from([(
        "recipe.json".to_owned(),
        br#"{"unbaked": 0, "output": {"kind": "video", "width": 16, "height": 16, "fps": "10", "duration_ms": 100}, "assets": {}, "layers": []}"#.to_vec(),
    )]);
    for files in [audio, video] {
        assert_eq!(
            render(&files, &NoFonts, limits).unwrap_err(),
            RenderError::TimedOut { limit_ms: 0 }
        );
    }
}

#[test]
fn webp_decodes_exactly_and_animated_webp_is_refused() {
    let file = include_bytes!("../../../tests/conformance/image-webp/package/assets/swatch.webp");
    let image = unbaked_render::image::decode_webp(file, 1000).unwrap();
    assert_eq!((image.width, image.height), (12, 10));
    for y in 0..10 {
        for x in 0..12 {
            let want = [x * 21, y * 25, 255 - x * 10, 255 - (x + y) * 12].map(|v| v as u8);
            assert_eq!(at(&image, x, y), want, "pixel {x},{y}");
        }
    }
    assert!(unbaked_render::image::decode_webp(file, 100).is_err());

    let mut animated =
        b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0\x02\0\0\0\x0b\0\0\x09\0\0ANIM\x06\0\0\0\0\0\0\0\0\0"
            .to_vec();
    let size = (animated.len() - 8) as u32;
    animated[4..8].copy_from_slice(&size.to_le_bytes());
    assert!(unbaked_render::image::decode_webp(&animated, 1000).is_err());
}

fn conformance_package(name: &str) -> pack::Files {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/conformance")
        .join(name)
        .join("package");
    pack::read_folder(&dir, Limits::default()).unwrap()
}

#[test]
fn previews_shrink_stills_and_grid_video_frames() {
    use unbaked_render::preview::{PreviewOptions, listen, preview};
    let limits = RenderLimits::default();
    let still = conformance_package("image-blend-modes");
    let full = preview(&still, &NoFonts, limits, PreviewOptions::default()).unwrap();
    assert_eq!((full.width, full.height), (48, 32));
    let small = PreviewOptions {
        max_edge: 24,
        ..PreviewOptions::default()
    };
    let small = preview(&still, &NoFonts, limits, small).unwrap();
    assert_eq!((small.width, small.height), (24, 16));

    let video = conformance_package("video-motion");
    let sheet = PreviewOptions {
        sheet: Some(4),
        ..PreviewOptions::default()
    };
    let grid = preview(&video, &NoFonts, limits, sheet).unwrap();
    // Two frames of 48x32 across and down, 4 pixels apart.
    assert_eq!((grid.width, grid.height), (100, 68));
    assert_eq!(at(&grid, 49, 10), [51, 51, 51, 255], "the gap");
    assert_eq!(at(&grid, 0, 0)[3], 255, "video frames are opaque");
    assert!(preview(&still, &NoFonts, limits, sheet).is_err());

    let sound = conformance_package("audio-mix");
    assert!(matches!(
        preview(&sound, &NoFonts, limits, PreviewOptions::default()),
        Err(RenderError::Unsupported(_))
    ));
    let stats = listen(&sound, limits).unwrap();
    assert_eq!(
        (stats.duration_ms, stats.sample_rate, stats.channels),
        (300, 48_000, 2)
    );
    assert!(stats.clipped_samples > 0, "the +12 dB clip clips");
    assert_eq!(stats.peak_dbfs, 0.0);
    assert_eq!(stats.loudness_dbfs.len(), 1);
    assert!(listen(&still, limits).is_err());
}

#[test]
fn estimates_read_sizes_from_the_recipe_and_headers() {
    use unbaked_core::recipe::OutputKind;
    use unbaked_render::estimate::estimate;

    let effects = estimate(&conformance_package("image-effects")).unwrap();
    assert_eq!(effects.kind, OutputKind::Image);
    assert_eq!((effects.canvas_pixels, effects.frames), (64 * 40, 1));
    // Five top-level layers and a group of two; five blurs or shadows and one group buffer.
    assert_eq!(
        (effects.layers, effects.blurs, effects.extra_buffers),
        (7, 5, 11)
    );
    assert_eq!(effects.max_blur_radius, 6, "sigma 2 on the group");
    assert_eq!(effects.image_pixels, 8 * 8 + 48 * 32);
    assert_eq!(effects.output_samples, 0);

    let motion = estimate(&conformance_package("video-motion")).unwrap();
    assert_eq!((motion.kind, motion.frames), (OutputKind::Video, 10));

    let clip = estimate(&conformance_package("video-clip")).unwrap();
    assert_eq!(clip.frames, 21, "700 ms at 30000/1001 fps");
    assert!(clip.video_pixels_per_frame > 0 && clip.video_pixels_per_frame.is_multiple_of(2));

    let mix = estimate(&conformance_package("audio-mix")).unwrap();
    assert_eq!((mix.canvas_pixels, mix.frames), (0, 0));
    assert_eq!(mix.output_samples, 300 * 48 * 2);
    // The mono tone, the stereo chord (each counted once) and the voice in whole AAC frames.
    let voice = mix.source_samples - 11_025 - 9_600 * 2;
    assert!(
        voice > 0 && voice.is_multiple_of(1024),
        "{}",
        mix.source_samples
    );
    assert!(mix.asset_bytes > 0);

    // A blur that would run for hours costs far more than the whole effects case.
    let mut hostile = conformance_package("image-effects");
    let recipe = String::from_utf8(hostile["recipe.json"].clone())
        .unwrap()
        .replace(r#""sigma": 2"#, r#""sigma": 400"#);
    hostile.insert("recipe.json".into(), recipe.into_bytes());
    let hostile = estimate(&hostile).unwrap();
    assert_eq!(hostile.max_blur_radius, 1200);
    assert!(
        hostile.work_units > 1000 * effects.work_units,
        "{hostile:?}"
    );
}
