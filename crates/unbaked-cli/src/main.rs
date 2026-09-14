//! `unbaked`: check, read, unpack and pack Unbaked media files.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use serde_json::{Value as Json, json};
use unbaked_core::bake::Change;
use unbaked_core::edit::{self, EditError};
use unbaked_core::json::Problem;
use unbaked_core::package::Limits;
use unbaked_core::{Status, open, pack};
use unbaked_render::preview::{self, PreviewOptions};
use unbaked_render::{Deadline, RenderError, RenderLimits};

mod fonts;

const HELP: &str = "\
unbaked: check, read, edit, unpack, pack and render Unbaked media files

Usage:
  unbaked check <file> [--json]         Is the render fresh, stale, render-modified or invalid?
  unbaked recipe <file>                 Print recipe.json
  unbaked unpack <file> <dir>           Write the package into an empty or new folder
  unbaked pack <dir> --into <file> [-o <out>]
                                        Put a folder's package into a file, replacing its
                                        package. Writes <file> in place unless -o is given.
                                        Keeps the file's bake.json if the folder has none.
  unbaked edit <file-or-dir> --patch <file|-> [-o <out>] [--json]
                                        Apply a JSON Patch (RFC 6902) to recipe.json. Nothing
                                        changes unless every operation applies and the result
                                        is valid. The render is left stale.
  unbaked add <file-or-dir> <media> --id <id> [--license <json-file>] [-o <out>] [--json]
                                        Pack an image, video, sound or font as assets/<id>.<ext>
                                        and point asset <id> at it, replacing its old file.
                                        A packed font needs --license ({\"spdx\": ...}).
                                        edit and add change a file or folder in place unless
                                        -o is given; -o for a folder names a new folder.
  unbaked render <file-or-dir> [-o <out>] [--fonts <dir>] [limits] [--json]
                                        Render the recipe and write a fresh Unbaked file.
                                        Renders a file in place unless -o is given; a
                                        folder needs -o. Images, sound and video.
                                        Fonts the recipe references but does not pack are
                                        looked up by SHA-256 in <dir> and its subfolders.

  unbaked preview <file-or-dir> -o <out.png> [--at-ms <n>] [--max-edge <n>] [--sheet <n>]
                [--fonts <dir>] [limits] [--json]
                                        Draw one moment of an image or video recipe as a plain
                                        PNG, longest edge at most 1024 unless --max-edge is
                                        given. --sheet: a grid of n evenly spaced video frames.
  unbaked listen <file-or-dir> [limits] [--json]
                                        Mix the sound and describe it: length, peak level,
                                        clipped samples, loudness per 500 ms, silent stretches.
  unbaked estimate <file-or-dir> [--json]
                                        How big a render is, from the recipe and asset headers
                                        only: pixels, frames, buffers, blur radius, samples,
                                        bytes, and one work-units number for pricing.

Limits (render, preview and listen):
  --time-limit-ms <n>                   Stop once the work has run this long
  --max-pixels <n>                      Most pixels in any one image buffer
  --max-samples <n>                     Most samples per channel in any one sound buffer
  --max-frames <n>                      Most frames in a video

With --json, the result is one object on stdout:
  {\"ok\": true, ...} or
  {\"ok\": false, \"error\": {\"kind\", \"message\", \"problems\": [{\"file\", \"path\", \"message\"}]}}

Exit codes:
  0  success (check: fresh)
  1  check: stale or render-modified
  2  invalid recipe or bake.json
  3  not an Unbaked file, its package breaks the format rules, or it cannot be rendered
  4  wrong arguments, or a file could not be read or written
  5  the time limit passed
";

/// How a command ended.
enum Fail {
    /// Wrong arguments.
    Usage(String),
    /// The recipe breaks the spec. Problems are `{file, path, message}` objects.
    Invalid(String, Vec<Json>),
    /// The file is not Unbaked or its package is broken.
    Format(String),
    /// The recipe cannot be rendered. The kind names why, as in [`render_kind`].
    Render(&'static str, String),
    /// Reading or writing failed.
    Io(String),
}

impl Fail {
    fn code(&self) -> u8 {
        match self {
            Fail::Usage(_) | Fail::Io(_) => 4,
            Fail::Invalid(..) => 2,
            Fail::Render("timed-out", _) => 5,
            Fail::Format(_) | Fail::Render(..) => 3,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Fail::Usage(_) => "usage",
            Fail::Invalid(..) => "invalid",
            Fail::Format(_) => "format",
            Fail::Render(kind, _) => kind,
            Fail::Io(_) => "io",
        }
    }

    fn message(&self) -> &str {
        match self {
            Fail::Usage(m)
            | Fail::Invalid(m, _)
            | Fail::Format(m)
            | Fail::Render(_, m)
            | Fail::Io(m) => m,
        }
    }

    /// The shape every command prints on failure with `--json`.
    fn json(&self) -> Json {
        let problems = match self {
            Fail::Invalid(_, problems) => problems.clone(),
            _ => Vec::new(),
        };
        json!({
            "ok": false,
            "error": { "kind": self.kind(), "message": self.message(), "problems": problems },
        })
    }
}

/// The error kind agents see for a render failure.
fn render_kind(e: &RenderError) -> &'static str {
    match e {
        RenderError::Recipe(_) => "invalid",
        RenderError::Unsupported(_) => "unsupported",
        RenderError::Decode { .. } => "decode",
        RenderError::FontNotFound { .. } => "font-not-found",
        RenderError::TooLarge { .. }
        | RenderError::TooLong { .. }
        | RenderError::TooManyFrames { .. } => "over-limit",
        RenderError::TimedOut { .. } => "timed-out",
        RenderError::Package(_) => "format",
        RenderError::Encode(_) => "encode",
    }
}

fn render_fail(input: &Path, e: RenderError) -> Fail {
    match e {
        RenderError::Recipe(problems) => Fail::Invalid(
            "recipe.json is invalid".into(),
            problems_json("recipe.json", &problems).collect(),
        ),
        other => Fail::Render(render_kind(&other), format!("{}: {other}", input.display())),
    }
}

impl From<lexopt::Error> for Fail {
    fn from(e: lexopt::Error) -> Self {
        Fail::Usage(e.to_string())
    }
}

fn main() -> ExitCode {
    let json_output = std::env::args_os().skip(2).any(|a| a == "--json");
    let code = match run() {
        Ok(code) => code,
        Err(fail) => {
            match &fail {
                _ if json_output => print_json(&fail.json()),
                Fail::Usage(message) => eprintln!("unbaked: {message}\n\n{HELP}"),
                Fail::Invalid(message, problems) => {
                    eprintln!("unbaked: {message}:");
                    for p in problems {
                        let text = |key: &str| p[key].as_str().unwrap_or_default().to_owned();
                        eprintln!("  {}: {}", text("path"), text("message"));
                    }
                }
                other => eprintln!("unbaked: {}", other.message()),
            }
            fail.code()
        }
    };
    ExitCode::from(code)
}

fn run() -> Result<u8, Fail> {
    use lexopt::prelude::*;
    let mut args = lexopt::Parser::from_env();
    let command = match args.next()? {
        Some(Value(cmd)) => cmd.string()?,
        Some(Long("help") | Short('h')) | None => {
            print!("{HELP}");
            return Ok(0);
        }
        Some(Long("version") | Short('V')) => {
            println!("unbaked {}", env!("CARGO_PKG_VERSION"));
            return Ok(0);
        }
        Some(other) => return Err(other.unexpected().into()),
    };

    let mut positional: Vec<OsString> = Vec::new();
    let mut json_output = false;
    let mut into: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut font_dir: Option<PathBuf> = None;
    let mut patch: Option<PathBuf> = None;
    let mut id: Option<String> = None;
    let mut license: Option<PathBuf> = None;
    let mut limits = RenderLimits::default();
    let mut time_limit_ms: Option<u64> = None;
    let limited = matches!(command.as_str(), "render" | "preview" | "listen");
    let mut options = PreviewOptions::default();
    while let Some(arg) = args.next()? {
        match arg {
            Value(v) => positional.push(v),
            Long("json") if command != "recipe" && command != "unpack" && command != "pack" => {
                json_output = true
            }
            Long("at-ms") if command == "preview" => options.at_ms = Some(args.value()?.parse()?),
            Long("max-edge") if command == "preview" => options.max_edge = args.value()?.parse()?,
            Long("sheet") if command == "preview" => options.sheet = Some(args.value()?.parse()?),
            Long("patch") if command == "edit" => patch = Some(args.value()?.into()),
            Long("id") if command == "add" => id = Some(args.value()?.string()?),
            Long("license") if command == "add" => license = Some(args.value()?.into()),
            Long("time-limit-ms") if limited => time_limit_ms = Some(args.value()?.parse()?),
            Long("max-pixels") if limited => limits.max_pixels = args.value()?.parse()?,
            Long("max-samples") if limited => limits.max_samples = args.value()?.parse()?,
            Long("max-frames") if limited => limits.max_frames = args.value()?.parse()?,
            Long("into") if command == "pack" => into = Some(args.value()?.into()),
            Short('o') | Long("output")
                if matches!(
                    command.as_str(),
                    "pack" | "render" | "edit" | "add" | "preview"
                ) =>
            {
                out = Some(args.value()?.into())
            }
            Long("fonts") if command == "render" || command == "preview" => {
                font_dir = Some(args.value()?.into())
            }
            Long("help") | Short('h') => {
                print!("{HELP}");
                return Ok(0);
            }
            other => return Err(other.unexpected().into()),
        }
    }
    let wanted = |n: usize| {
        if positional.len() == n {
            Ok(positional.iter().map(PathBuf::from).collect::<Vec<_>>())
        } else {
            Err(Fail::Usage(format!(
                "{command} takes {n} path{}, got {}",
                if n == 1 { "" } else { "s" },
                positional.len()
            )))
        }
    };

    match command.as_str() {
        "check" => check(&wanted(1)?[0], json_output),
        "recipe" => recipe(&wanted(1)?[0]),
        "unpack" => {
            let paths = wanted(2)?;
            unpack(&paths[0], &paths[1])
        }
        "pack" => {
            let paths = wanted(1)?;
            let into = into.ok_or_else(|| Fail::Usage("pack needs --into <file>".into()))?;
            pack_into(&paths[0], &into, out.as_deref().unwrap_or(&into))
        }
        "edit" => {
            let patch = patch.ok_or_else(|| Fail::Usage("edit needs --patch <file|->".into()))?;
            let input = &wanted(1)?[0];
            let files = load(input)?;
            let patch = if patch.as_os_str() == "-" {
                let mut bytes = Vec::new();
                io::Read::read_to_end(&mut io::stdin(), &mut bytes)
                    .map_err(|e| Fail::Io(format!("stdin: {e}")))?;
                bytes
            } else {
                read(&patch)?
            };
            let changed = edit::edit(&files, &patch).map_err(edit_fail)?;
            save(input, &files, &changed, out.as_deref(), json_output)
        }
        "add" => {
            let id = id.ok_or_else(|| Fail::Usage("add needs --id <id>".into()))?;
            let paths = wanted(2)?;
            let files = load(&paths[0])?;
            let license = match license {
                Some(path) => Some(
                    unbaked_core::json::parse(&read(&path)?)
                        .map_err(|p| Fail::Usage(format!("{}: {}", path.display(), p.message)))?,
                ),
                None => None,
            };
            let media = read(&paths[1])?;
            let changed = edit::add_asset(&files, &id, &media, license).map_err(edit_fail)?;
            save(&paths[0], &files, &changed, out.as_deref(), json_output)
        }
        "preview" => {
            limits.deadline = time_limit_ms.map(Deadline::after_ms);
            let input = &wanted(1)?[0];
            let out = out.ok_or_else(|| Fail::Usage("preview needs -o <out.png>".into()))?;
            let started = Instant::now();
            let files = load(input)?;
            let fonts = font_source(font_dir)?;
            let pixels = preview::preview(&files, fonts.as_ref(), limits, options)
                .map_err(|e| render_fail(input, e))?;
            let png =
                unbaked_render::image::encode_png(pixels.width, pixels.height, &pixels.to_rgba8())
                    .map_err(|e| Fail::Render("encode", e))?;
            write_replacing(&out, &png)?;
            if json_output {
                print_json(&json!({
                    "ok": true,
                    "output": out.display().to_string(),
                    "width": pixels.width,
                    "height": pixels.height,
                    "elapsed_ms": started.elapsed().as_millis() as u64,
                }));
            } else {
                eprintln!(
                    "wrote {} ({}x{})",
                    out.display(),
                    pixels.width,
                    pixels.height
                );
            }
            Ok(0)
        }
        "estimate" => {
            let input = &wanted(1)?[0];
            let files = load(input)?;
            let e =
                unbaked_render::estimate::estimate(&files).map_err(|e| render_fail(input, e))?;
            let kind = match e.kind {
                unbaked_core::recipe::OutputKind::Image => "image",
                unbaked_core::recipe::OutputKind::Video => "video",
                unbaked_core::recipe::OutputKind::Audio => "audio",
            };
            let report = json!({
                "ok": true,
                "kind": kind,
                "canvas_pixels": e.canvas_pixels,
                "frames": e.frames,
                "layers": e.layers,
                "extra_buffers": e.extra_buffers,
                "blurs": e.blurs,
                "max_blur_radius": e.max_blur_radius,
                "image_pixels": e.image_pixels,
                "video_pixels_per_frame": e.video_pixels_per_frame,
                "output_samples": e.output_samples,
                "source_samples": e.source_samples,
                "asset_bytes": e.asset_bytes,
                "work_units": e.work_units,
            });
            if json_output {
                print_json(&report);
            } else if let Json::Object(fields) = &report {
                for (name, value) in fields.iter().skip(1) {
                    println!(
                        "{name}: {}",
                        value
                            .as_str()
                            .map_or_else(|| value.to_string(), str::to_owned)
                    );
                }
            }
            Ok(0)
        }
        "listen" => {
            limits.deadline = time_limit_ms.map(Deadline::after_ms);
            let input = &wanted(1)?[0];
            let files = load(input)?;
            let stats = preview::listen(&files, limits).map_err(|e| render_fail(input, e))?;
            // Minus infinity (silence) has no JSON number; it becomes null.
            let level = |db: f64| {
                if db.is_finite() {
                    json!((db * 10.0).round() / 10.0)
                } else {
                    Json::Null
                }
            };
            let report = json!({
                "ok": true,
                "duration_ms": stats.duration_ms,
                "sample_rate": stats.sample_rate,
                "channels": stats.channels,
                "peak_dbfs": level(stats.peak_dbfs),
                "clipped_samples": stats.clipped_samples,
                "window_ms": preview::WINDOW_MS,
                "loudness_dbfs": stats.loudness_dbfs.iter().map(|&db| level(db)).collect::<Vec<_>>(),
                "silences": stats.silences.iter().map(|s| json!({"start_ms": s.start_ms, "end_ms": s.end_ms})).collect::<Vec<_>>(),
            });
            if json_output {
                print_json(&report);
            } else {
                let text = |v: &Json| {
                    if v.is_null() {
                        "silent".to_owned()
                    } else {
                        format!("{v} dBFS")
                    }
                };
                println!(
                    "{} ms, {} Hz, {} channel(s)",
                    stats.duration_ms, stats.sample_rate, stats.channels
                );
                println!(
                    "peak {}, {} clipped samples",
                    text(&report["peak_dbfs"]),
                    stats.clipped_samples
                );
                for (i, v) in report["loudness_dbfs"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    println!("  {:>6} ms  {}", i as u64 * preview::WINDOW_MS, text(v));
                }
                for s in &stats.silences {
                    println!("silent {}-{} ms", s.start_ms, s.end_ms);
                }
            }
            Ok(0)
        }
        "render" => {
            // The clock starts once the arguments are read.
            limits.deadline = time_limit_ms.map(Deadline::after_ms);
            render(
                &wanted(1)?[0],
                out.as_deref(),
                font_dir,
                limits,
                json_output,
            )
        }
        other => Err(Fail::Usage(format!("unknown command {other:?}"))),
    }
}

fn render(
    input: &Path,
    out: Option<&Path>,
    font_dir: Option<PathBuf>,
    limits: RenderLimits,
    json_output: bool,
) -> Result<u8, Fail> {
    let started = Instant::now();
    let (files, out) = if input.is_dir() {
        let out = out.ok_or_else(|| Fail::Usage("render of a folder needs -o <out>".into()))?;
        let files = pack::read_folder(input, Limits::default()).map_err(|e| match e {
            pack::FolderError::Io { .. } => Fail::Io(e.to_string()),
            other => Fail::Format(format!("{}: {other}", input.display())),
        })?;
        (files, out)
    } else {
        (opened_files(input)?, out.unwrap_or(input))
    };
    let fonts = font_source(font_dir)?;
    let file = unbaked_render::render(&files, fonts.as_ref(), limits)
        .map_err(|e| render_fail(input, e))?;
    write_replacing(out, &file)?;
    if json_output {
        print_json(&json!({
            "ok": true,
            "output": out.display().to_string(),
            "bytes": file.len(),
            "elapsed_ms": started.elapsed().as_millis() as u64,
        }));
    } else {
        eprintln!("rendered {}", out.display());
    }
    Ok(0)
}

fn edit_fail(e: EditError) -> Fail {
    match e {
        EditError::Patch(problems) => Fail::Invalid(
            "the patch cannot be applied".into(),
            problems_json("patch", &problems).collect(),
        ),
        EditError::Recipe(problems) => Fail::Invalid(
            "the edited recipe.json is invalid".into(),
            problems_json("recipe.json", &problems).collect(),
        ),
        EditError::Asset(message) => Fail::Render("unsupported", message),
    }
}

/// Fonts looked up by SHA-256 in a folder, or none.
fn font_source(font_dir: Option<PathBuf>) -> Result<Box<dyn unbaked_render::FontSource>, Fail> {
    Ok(match font_dir {
        Some(dir) if !dir.is_dir() => {
            return Err(Fail::Io(format!("{}: not a folder", dir.display())));
        }
        Some(dir) => Box::new(fonts::FontFolder::new(dir)),
        None => Box::new(unbaked_render::NoFonts),
    })
}

/// The package of an Unbaked file, or of a folder in the directory form.
fn load(input: &Path) -> Result<pack::Files, Fail> {
    if input.is_dir() {
        pack::read_folder(input, Limits::default()).map_err(|e| match e {
            pack::FolderError::Io { .. } => Fail::Io(e.to_string()),
            other => Fail::Format(format!("{}: {other}", input.display())),
        })
    } else {
        opened_files(input)
    }
}

/// Writes changed package files back: into the file's package, or into the
/// folder file by file. With `out`, a file goes to `out` and a folder to the
/// new folder `out`.
fn save(
    input: &Path,
    before: &pack::Files,
    after: &pack::Files,
    out: Option<&Path>,
    json_output: bool,
) -> Result<u8, Fail> {
    let target = out.unwrap_or(input);
    if input.is_dir() {
        if let Some(out) = out {
            pack::write_folder(after, out).map_err(|e| match e {
                pack::FolderError::Io { .. } | pack::FolderError::NotEmpty(_) => {
                    Fail::Io(e.to_string())
                }
                other => Fail::Format(other.to_string()),
            })?;
        } else {
            for (name, data) in after {
                if before.get(name) != Some(data) {
                    let path = input.join(name);
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent)
                            .map_err(|e| Fail::Io(format!("{}: {e}", parent.display())))?;
                    }
                    write_replacing(&path, data)?;
                }
            }
            for name in before.keys().filter(|name| !after.contains_key(*name)) {
                let path = input.join(name);
                fs::remove_file(&path).map_err(|e| Fail::Io(format!("{}: {e}", path.display())))?;
            }
        }
    } else {
        let package = pack::write(after, Limits::default())
            .map_err(|e| Fail::Format(format!("{}: {e}", input.display())))?;
        let result = pack::with_package(&read(input)?, &package)
            .map_err(|e| Fail::Format(format!("{}: {e}", input.display())))?;
        write_replacing(target, &result)?;
    }
    if json_output {
        print_json(&json!({ "ok": true, "output": target.display().to_string() }));
    } else {
        eprintln!(
            "updated {}; render it to refresh the visible media",
            target.display()
        );
    }
    Ok(0)
}

fn read(path: &Path) -> Result<Vec<u8>, Fail> {
    fs::read(path).map_err(|e| Fail::Io(format!("{}: {e}", path.display())))
}

fn check(path: &Path, json_output: bool) -> Result<u8, Fail> {
    let bytes = read(path)?;
    let opened = match open(&bytes, Limits::default()) {
        Ok(opened) => opened,
        Err(e) if json_output => {
            let mut report = Fail::Format(format!("{}: {e}", path.display())).json();
            report["status"] = "not-unbaked".into();
            print_json(&report);
            return Ok(3);
        }
        Err(e) => return Err(Fail::Format(format!("{}: {e}", path.display()))),
    };
    let status = opened.check();
    let code = match &status {
        Status::Fresh => 0,
        Status::Stale(_) | Status::RenderModified => 1,
        Status::Invalid { .. } => 2,
    };
    if json_output {
        let mut report = json!({ "ok": true });
        if let (Some(all), Json::Object(fields)) = (report.as_object_mut(), status_json(&status)) {
            all.extend(fields);
        }
        print_json(&report);
    } else {
        print!("{}", status_text(&status));
    }
    Ok(code)
}

fn change_json(change: &Change) -> Json {
    match change {
        Change::Recipe => json!({ "kind": "recipe" }),
        Change::Carrier => json!({ "kind": "carrier" }),
        Change::AssetChanged(p) => json!({ "kind": "asset-changed", "path": p }),
        Change::AssetAdded(p) => json!({ "kind": "asset-added", "path": p }),
        Change::AssetRemoved(p) => json!({ "kind": "asset-removed", "path": p }),
    }
}

fn problems_json(file: &str, problems: &[Problem]) -> impl Iterator<Item = Json> {
    problems
        .iter()
        .map(move |p| json!({ "file": file, "path": p.path, "message": p.message }))
}

/// The stable shape agents read. Paths are JSON Pointers into the named file.
fn status_json(status: &Status) -> Json {
    match status {
        Status::Fresh => json!({ "status": "fresh" }),
        Status::RenderModified => json!({ "status": "render-modified" }),
        Status::Stale(changes) => json!({
            "status": "stale",
            "changes": changes.iter().map(change_json).collect::<Vec<_>>(),
        }),
        Status::Invalid { recipe, bake } => json!({
            "status": "invalid",
            "problems": problems_json("recipe.json", recipe)
                .chain(problems_json("bake.json", bake))
                .collect::<Vec<_>>(),
        }),
    }
}

fn status_text(status: &Status) -> String {
    match status {
        Status::Fresh => "fresh: the render matches the recipe and assets\n".into(),
        Status::RenderModified => {
            "render-modified: the recipe and assets match, but the visible media was edited outside Unbaked\n".into()
        }
        Status::Stale(changes) => {
            let mut out = String::from("stale: render again to update the visible media\n");
            for change in changes {
                let line = match change {
                    Change::Recipe => "recipe.json changed".to_owned(),
                    Change::Carrier => "output.kind needs a different file type".to_owned(),
                    Change::AssetChanged(p) => format!("{p} changed"),
                    Change::AssetAdded(p) => format!("{p} is new"),
                    Change::AssetRemoved(p) => format!("{p} is no longer used"),
                };
                out.push_str(&format!("  {line}\n"));
            }
            out
        }
        Status::Invalid { recipe, bake } => {
            let mut out = String::from("invalid:\n");
            for (file, problems) in [("recipe.json", recipe), ("bake.json", bake)] {
                for p in problems {
                    let at = if p.path.is_empty() { "" } else { " " };
                    out.push_str(&format!("  {file}{at}{}: {}\n", p.path, p.message));
                }
            }
            out
        }
    }
}

fn print_json(value: &Json) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("JSON values serialise")
    );
}

fn opened_files(path: &Path) -> Result<pack::Files, Fail> {
    let bytes = read(path)?;
    pack::read_files(&bytes, Limits::default())
        .map_err(|e| Fail::Format(format!("{}: {e}", path.display())))
}

fn recipe(path: &Path) -> Result<u8, Fail> {
    let files = opened_files(path)?;
    let recipe = &files["recipe.json"];
    io::stdout()
        .write_all(recipe)
        .map_err(|e| Fail::Io(e.to_string()))?;
    Ok(0)
}

fn unpack(path: &Path, dir: &Path) -> Result<u8, Fail> {
    let files = opened_files(path)?;
    pack::write_folder(&files, dir).map_err(|e| match e {
        pack::FolderError::Io { .. } | pack::FolderError::NotEmpty(_) => Fail::Io(e.to_string()),
        other => Fail::Format(other.to_string()),
    })?;
    eprintln!("unpacked {} files into {}", files.len(), dir.display());
    Ok(0)
}

fn pack_into(dir: &Path, into: &Path, out: &Path) -> Result<u8, Fail> {
    let mut files = pack::read_folder(dir, Limits::default()).map_err(|e| match e {
        pack::FolderError::Io { .. } => Fail::Io(e.to_string()),
        other => Fail::Format(format!("{}: {other}", dir.display())),
    })?;
    let carrier = read(into)?;
    if !files.contains_key("bake.json") {
        // The directory form may leave bake.json out; keep the render's fingerprints.
        let old = pack::read_files(&carrier, Limits::default()).map_err(|e| {
            Fail::Format(format!(
                "{} has no bake.json, and {} has none to keep: {e}",
                dir.display(),
                into.display()
            ))
        })?;
        files.insert("bake.json".into(), old["bake.json"].clone());
    }
    let package = pack::write(&files, Limits::default())
        .map_err(|e| Fail::Format(format!("{}: {e}", dir.display())))?;
    let result = pack::with_package(&carrier, &package)
        .map_err(|e| Fail::Format(format!("{}: {e}", into.display())))?;
    write_replacing(out, &result)?;
    eprintln!("packed {} files into {}", files.len(), out.display());
    Ok(0)
}

/// Writes to a temporary file beside `path`, then renames it over `path`, so a
/// failed write never leaves a half-written file behind.
fn write_replacing(path: &Path, bytes: &[u8]) -> Result<(), Fail> {
    let io_err = |e: io::Error| Fail::Io(format!("{}: {e}", path.display()));
    let mut temp_name = path.file_name().unwrap_or_default().to_owned();
    temp_name.push(".unbaked-tmp");
    let temp = path.with_file_name(temp_name);
    let written = fs::File::create(&temp)
        .and_then(|mut f| f.write_all(bytes).and_then(|()| f.sync_all()))
        .and_then(|()| fs::rename(&temp, path));
    if let Err(e) = written {
        let _ = fs::remove_file(&temp);
        return Err(io_err(e));
    }
    Ok(())
}
