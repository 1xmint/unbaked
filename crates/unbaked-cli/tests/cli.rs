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
    assert_eq!(json(&out), serde_json::json!({ "status": "fresh" }));

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
        serde_json::json!({ "status": "stale", "changes": [ { "kind": "recipe" } ] })
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
    assert_eq!(unbaked(&[&"check", &plain]).status.code(), Some(3));

    assert_eq!(unbaked(&[&"check"]).status.code(), Some(4));
    assert_eq!(unbaked(&[&"frobnicate", &plain]).status.code(), Some(4));
    assert_eq!(
        unbaked(&[&"check", &s.0.join("missing.png")]).status.code(),
        Some(4)
    );
    assert_eq!(unbaked(&[&"--help"]).status.code(), Some(0));
}
