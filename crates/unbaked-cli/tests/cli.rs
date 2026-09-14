//! Runs the real `unbaked` binary on files built here.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use unbaked_core::package::Limits;
use unbaked_core::{pack, sha256_hex};

fn crc32(parts: &[&[u8]]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for part in parts {
        for &byte in *part {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    0xEDB8_8320 ^ (crc >> 1)
                } else {
                    crc >> 1
                };
            }
        }
    }
    !crc
}

/// A valid 1x1 RGBA PNG holding one pixel.
fn png_pixel(rgba: [u8; 4]) -> Vec<u8> {
    let chunk = |kind: &[u8; 4], data: &[u8]| {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
        out
    };
    let raw = [0, rgba[0], rgba[1], rgba[2], rgba[3]];
    let (mut a, mut b) = (1u32, 0u32);
    for byte in raw {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    let mut idat = vec![0x78, 0x01, 0x01, 0x05, 0x00, 0xfa, 0xff];
    idat.extend_from_slice(&raw);
    idat.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend(chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]));
    png.extend(chunk(b"IDAT", &idat));
    png.extend(chunk(b"IEND", &[]));
    png
}

const RECIPE: &str = r##"{
  "unbaked": 0,
  "output": { "kind": "image", "width": 1, "height": 1 },
  "assets": { "logo": { "path": "assets/logo.png" } },
  "layers": [ { "id": "logo", "type": "image", "asset": "logo" } ]
}
"##;

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("unbaked-cli-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Writes a fresh `poster.unbaked.png` built from `recipe` into `dir`.
fn fresh_file(dir: &Path, recipe: &str) -> PathBuf {
    let render = png_pixel([10, 20, 30, 255]);
    let logo = png_pixel([1, 2, 3, 4]);
    let bake = format!(
        r#"{{"unbaked": 0, "renderer": "test", "recipe_sha256": "{}", "assets_sha256": {{"assets/logo.png": "{}"}}, "render_sha256": "{}"}}"#,
        sha256_hex(recipe.as_bytes()),
        sha256_hex(&logo),
        sha256_hex(&render)
    );
    let files: pack::Files = [
        ("recipe.json", recipe.as_bytes().to_vec()),
        ("bake.json", bake.into_bytes()),
        ("assets/logo.png", logo),
    ]
    .into_iter()
    .map(|(n, d)| (n.to_owned(), d))
    .collect();
    let package = pack::write(&files, Limits::default()).unwrap();
    let path = dir.join("poster.unbaked.png");
    fs::write(&path, pack::with_package(&render, &package).unwrap()).unwrap();
    path
}

fn unbaked(args: &[&dyn AsRef<std::ffi::OsStr>]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_unbaked"))
        .args(args.iter().map(|a| a.as_ref()))
        .output()
        .unwrap()
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!("{e}: {}", String::from_utf8_lossy(&out.stdout));
    })
}

#[test]
fn check_reports_fresh_in_text_and_json() {
    let s = Scratch::new("fresh");
    let file = fresh_file(&s.0, RECIPE);

    let out = unbaked(&[&"check", &file]);
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("fresh"));

    let out = unbaked(&[&"check", &file, &"--json"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        json(&out),
        serde_json::json!({ "ok": true, "status": "fresh" })
    );

    let out = unbaked(&[&"recipe", &file]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(out.stdout, RECIPE.as_bytes());
}

#[test]
fn unpack_edit_pack_gives_a_stale_file() {
    let s = Scratch::new("round");
    let file = fresh_file(&s.0, RECIPE);
    let folder = s.0.join("poster");

    let out = unbaked(&[&"unpack", &file, &folder]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let recipe_path = folder.join("recipe.json");
    assert_eq!(fs::read_to_string(&recipe_path).unwrap(), RECIPE);

    // Edit the recipe and drop bake.json, as an editor of the folder form might.
    fs::write(
        &recipe_path,
        RECIPE.replace("\"id\": \"logo\"", "\"id\": \"mark\""),
    )
    .unwrap();
    fs::remove_file(folder.join("bake.json")).unwrap();
    let edited = s.0.join("edited.unbaked.png");
    let out = unbaked(&[&"pack", &folder, &"--into", &file, &"-o", &edited]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");

    let out = unbaked(&[&"check", &edited, &"--json"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        json(&out),
        serde_json::json!({ "ok": true, "status": "stale", "changes": [ { "kind": "recipe" } ] })
    );
    // The original is untouched when -o is given.
    assert_eq!(unbaked(&[&"check", &file]).status.code(), Some(0));

    // Unpacking into a folder that is not empty is refused.
    let out = unbaked(&[&"unpack", &file, &folder]);
    assert_eq!(out.status.code(), Some(4));
}

#[test]
fn pack_without_o_replaces_the_file_in_place() {
    let s = Scratch::new("inplace");
    let file = fresh_file(&s.0, RECIPE);
    let folder = s.0.join("poster");
    assert_eq!(unbaked(&[&"unpack", &file, &folder]).status.code(), Some(0));
    fs::write(
        folder.join("recipe.json"),
        RECIPE.replace("\"width\": 1", "\"width\": 2"),
    )
    .unwrap();
    assert_eq!(
        unbaked(&[&"pack", &folder, &"--into", &file]).status.code(),
        Some(0)
    );
    assert_eq!(unbaked(&[&"check", &file]).status.code(), Some(1));
    let leftovers: Vec<_> = fs::read_dir(&s.0)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(
        leftovers.len(),
        2,
        "no temporary file left behind: {leftovers:?}"
    );
}

#[test]
fn render_makes_a_stale_file_fresh_again() {
    let s = Scratch::new("render");
    let file = fresh_file(&s.0, RECIPE);
    let folder = s.0.join("poster");
    assert_eq!(unbaked(&[&"unpack", &file, &folder]).status.code(), Some(0));
    fs::write(
        folder.join("recipe.json"),
        RECIPE.replace("\"width\": 1", "\"width\": 3"),
    )
    .unwrap();
    assert_eq!(
        unbaked(&[&"pack", &folder, &"--into", &file]).status.code(),
        Some(0)
    );
    assert_eq!(unbaked(&[&"check", &file]).status.code(), Some(1));

    let out = unbaked(&[&"render", &file]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let out = unbaked(&[&"check", &file, &"--json"]);
    assert_eq!(
        json(&out),
        serde_json::json!({ "ok": true, "status": "fresh" })
    );

    // A folder renders straight to a new file.
    let from_folder = s.0.join("from-folder.unbaked.png");
    let out = unbaked(&[&"render", &folder, &"-o", &from_folder]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(unbaked(&[&"check", &from_folder]).status.code(), Some(0));
    assert_eq!(unbaked(&[&"render", &folder]).status.code(), Some(4));

    // A broken recipe is reported as invalid, and nothing is written.
    fs::write(folder.join("recipe.json"), "{}").unwrap();
    let broken = s.0.join("broken.unbaked.png");
    let out = unbaked(&[&"render", &folder, &"-o", &broken]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(!broken.exists());
}

#[test]
fn render_finds_referenced_fonts_in_a_folder() {
    let s = Scratch::new("fonts");
    let lato = include_bytes!("../../../tests/fonts/Lato-Regular.ttf");
    let folder = s.0.join("card");
    fs::create_dir_all(&folder).unwrap();
    let recipe = format!(
        r#"{{"unbaked": 0, "output": {{"kind": "image", "width": 40, "height": 30}},
            "assets": {{"lato": {{"ref": {{"family": "Lato", "sha256": "{}"}}}}}},
            "layers": [{{"id": "t", "type": "text", "text": "Hi", "font": "lato", "size_px": 20}}]}}"#,
        sha256_hex(lato)
    );
    fs::write(folder.join("recipe.json"), recipe).unwrap();
    let out_file = s.0.join("card.unbaked.png");

    let out = unbaked(&[&"render", &folder, &"-o", &out_file]);
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    let message = String::from_utf8_lossy(&out.stderr);
    assert!(message.contains("font \"Lato\" was not found"), "{message}");

    // Found by fingerprint in a subfolder, whatever the file is called.
    let fonts = s.0.join("fonts");
    fs::create_dir_all(fonts.join("nested")).unwrap();
    fs::write(fonts.join("nested").join("renamed.OTF"), lato).unwrap();
    fs::write(fonts.join("decoy.ttf"), b"not the font").unwrap();
    let out = unbaked(&[&"render", &folder, &"-o", &out_file, &"--fonts", &fonts]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(unbaked(&[&"check", &out_file]).status.code(), Some(0));

    let missing = s.0.join("no-such-folder");
    let out = unbaked(&[&"render", &folder, &"-o", &out_file, &"--fonts", &missing]);
    assert_eq!(out.status.code(), Some(4), "{out:?}");
}

#[test]
fn invalid_and_foreign_files_get_their_own_exit_codes() {
    let s = Scratch::new("bad");
    let broken = fresh_file(
        &s.0,
        &RECIPE.replace("\"asset\": \"logo\"", "\"asset\": \"nope\""),
    );
    let out = unbaked(&[&"check", &broken, &"--json"]);
    assert_eq!(out.status.code(), Some(2));
    let report = json(&out);
    assert_eq!(report["status"], "invalid");
    assert_eq!(report["problems"][0]["file"], "recipe.json");
    assert_eq!(report["problems"][0]["path"], "/layers/0/asset");

    let plain = s.0.join("plain.png");
    fs::write(&plain, png_pixel([0, 0, 0, 255])).unwrap();
    let out = unbaked(&[&"check", &plain, &"--json"]);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(json(&out)["status"], "not-unbaked");
    assert_eq!(json(&out)["ok"], false);
    assert_eq!(json(&out)["error"]["kind"], "format");
    assert_eq!(unbaked(&[&"check", &plain]).status.code(), Some(3));

    assert_eq!(unbaked(&[&"check"]).status.code(), Some(4));
    assert_eq!(unbaked(&[&"frobnicate", &plain]).status.code(), Some(4));
    assert_eq!(
        unbaked(&[&"check", &s.0.join("missing.png")]).status.code(),
        Some(4)
    );
    assert_eq!(unbaked(&[&"--help"]).status.code(), Some(0));
}

#[test]
fn render_json_reports_results_limits_and_errors() {
    let s = Scratch::new("limits");
    let folder = s.0.join("wall");
    fs::create_dir_all(&folder).unwrap();
    let out_file = s.0.join("wall.unbaked.png");
    let recipe = |size: u32, sigma: u32| {
        format!(
            r##"{{"unbaked": 0, "output": {{"kind": "image", "width": {size}, "height": {size}}}, "assets": {{}},
                "layers": [{{"id": "wall", "type": "solid", "color": "#ffffffff", "width": {size}, "height": {size},
                            "effects": [{{"type": "blur", "sigma": {sigma}}}]}}]}}"##
        )
    };

    fs::write(folder.join("recipe.json"), recipe(4, 1)).unwrap();
    let out = unbaked(&[&"render", &folder, &"-o", &out_file, &"--json"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let report = json(&out);
    assert_eq!(report["ok"], true);
    assert!(report["elapsed_ms"].is_u64() && report["bytes"].as_u64().unwrap() > 0);

    let out = unbaked(&[
        &"render",
        &folder,
        &"-o",
        &out_file,
        &"--max-pixels",
        &"8",
        &"--json",
    ]);
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    assert_eq!(json(&out)["error"]["kind"], "over-limit");

    // Hours of blurring inside the pixel limit, stopped by the clock.
    fs::write(folder.join("recipe.json"), recipe(1500, 100)).unwrap();
    let args: [&dyn AsRef<std::ffi::OsStr>; 7] = [
        &"render",
        &folder,
        &"-o",
        &out_file,
        &"--time-limit-ms",
        &"50",
        &"--json",
    ];
    let out = unbaked(&args);
    assert_eq!(out.status.code(), Some(5), "{out:?}");
    assert_eq!(
        json(&out),
        serde_json::json!({ "ok": false, "error": {
            "kind": "timed-out",
            "message": format!("{}: the render took longer than the limit of 50 ms", folder.display()),
            "problems": [],
        }})
    );
    let out = unbaked(&args[..6]);
    assert_eq!(out.status.code(), Some(5), "{out:?}");

    fs::write(folder.join("recipe.json"), r#"{"unbaked": 0}"#).unwrap();
    let out = unbaked(&[&"render", &folder, &"-o", &out_file, &"--json"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let report = json(&out);
    assert_eq!(report["error"]["kind"], "invalid");
    assert_eq!(report["error"]["problems"][0]["file"], "recipe.json");

    let out = unbaked(&[&"render", &folder, &"--time-limit-ms", &"soon", &"--json"]);
    assert_eq!(out.status.code(), Some(4), "{out:?}");
    assert_eq!(json(&out)["error"]["kind"], "usage");
}

#[test]
fn edit_then_render_makes_the_file_fresh_again() {
    let s = Scratch::new("edit");
    let file = fresh_file(&s.0, RECIPE);
    let patch = s.0.join("patch.json");

    fs::write(
        &patch,
        r#"[{"op": "add", "path": "/layers/0/opacity", "value": 0.5}]"#,
    )
    .unwrap();
    let out = unbaked(&[&"edit", &file, &"--patch", &patch, &"--json"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(json(&out)["ok"], true);
    let out = unbaked(&[&"check", &file, &"--json"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(json(&out)["changes"][0]["kind"], "recipe");
    assert_eq!(unbaked(&[&"render", &file]).status.code(), Some(0));
    assert_eq!(unbaked(&[&"check", &file]).status.code(), Some(0));
    let recipe = String::from_utf8(unbaked(&[&"recipe", &file]).stdout).unwrap();
    assert!(recipe.contains("\"opacity\": 0.5"), "{recipe}");

    // A failing operation or an invalid result changes nothing.
    let before = fs::read(&file).unwrap();
    fs::write(
        &patch,
        r#"[{"op": "test", "path": "/layers/0/id", "value": "other"}]"#,
    )
    .unwrap();
    let out = unbaked(&[&"edit", &file, &"--patch", &patch, &"--json"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let report = json(&out);
    assert_eq!(report["error"]["problems"][0]["file"], "patch");
    assert_eq!(report["error"]["problems"][0]["path"], "/0");
    fs::write(
        &patch,
        r#"[{"op": "replace", "path": "/layers/0/asset", "value": "gone"}]"#,
    )
    .unwrap();
    let out = unbaked(&[&"edit", &file, &"--patch", &patch, &"--json"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let problem = &json(&out)["error"]["problems"][0];
    assert_eq!(
        (&problem["file"], &problem["path"]),
        (&"recipe.json".into(), &"/layers/0/asset".into())
    );
    assert_eq!(fs::read(&file).unwrap(), before);
    assert_eq!(unbaked(&[&"edit", &file]).status.code(), Some(4));
}

#[test]
fn add_packs_media_into_a_folder_or_file() {
    let s = Scratch::new("add");
    let file = fresh_file(&s.0, RECIPE);
    let folder = s.0.join("unpacked");
    assert_eq!(unbaked(&[&"unpack", &file, &folder]).status.code(), Some(0));
    let media = s.0.join("new-logo.png");
    fs::write(&media, png_pixel([200, 100, 50, 255])).unwrap();

    let out = unbaked(&[&"add", &folder, &media, &"--id", &"logo", &"--json"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        fs::read(folder.join("assets/logo.png")).unwrap(),
        png_pixel([200, 100, 50, 255])
    );
    let out = unbaked(&[&"add", &folder, &media, &"--id", &"bg"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let recipe = fs::read_to_string(folder.join("recipe.json")).unwrap();
    assert!(recipe.contains("\"path\": \"assets/bg.png\""), "{recipe}");

    let into_file = s.0.join("copy.unbaked.png");
    let out = unbaked(&[&"add", &file, &media, &"--id", &"extra", &"-o", &into_file]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(unbaked(&[&"check", &into_file]).status.code(), Some(1));

    let text = s.0.join("notes.txt");
    fs::write(&text, "not media").unwrap();
    let out = unbaked(&[&"add", &folder, &text, &"--id", &"notes", &"--json"]);
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    assert_eq!(json(&out)["error"]["kind"], "unsupported");
    assert_eq!(unbaked(&[&"add", &folder, &media]).status.code(), Some(4));
}
