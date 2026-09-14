//! `unbaked`: check, read, unpack and pack Unbaked media files.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use serde_json::{Value as Json, json};
use unbaked_core::bake::Change;
use unbaked_core::json::Problem;
use unbaked_core::package::Limits;
use unbaked_core::{Status, open, pack};
use unbaked_render::{Deadline, RenderError, RenderLimits};

mod fonts;

const HELP: &str = "\
unbaked: check, read, unpack, pack and render Unbaked media files

Usage:
  unbaked check <file> [--json]         Is the render fresh, stale, render-modified or invalid?
  unbaked recipe <file>                 Print recipe.json
  unbaked unpack <file> <dir>           Write the package into an empty or new folder
  unbaked pack <dir> --into <file> [-o <out>]
                                        Put a folder's package into a file, replacing its
                                        package. Writes <file> in place unless -o is given.
                                        Keeps the file's bake.json if the folder has none.
  unbaked render <file-or-dir> [-o <out>] [--fonts <dir>] [limits] [--json]
                                        Render the recipe and write a fresh Unbaked file.
                                        Renders a file in place unless -o is given; a
                                        folder needs -o. Images, sound and video.
                                        Fonts the recipe references but does not pack are
                                        looked up by SHA-256 in <dir> and its subfolders.

Limits:
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
    let mut limits = RenderLimits::default();
    let mut time_limit_ms: Option<u64> = None;
    let limited = command == "render";
    while let Some(arg) = args.next()? {
        match arg {
            Value(v) => positional.push(v),
            Long("json") if command == "check" || command == "render" => json_output = true,
            Long("time-limit-ms") if limited => time_limit_ms = Some(args.value()?.parse()?),
            Long("max-pixels") if limited => limits.max_pixels = args.value()?.parse()?,
            Long("max-samples") if limited => limits.max_samples = args.value()?.parse()?,
            Long("max-frames") if limited => limits.max_frames = args.value()?.parse()?,
            Long("into") if command == "pack" => into = Some(args.value()?.into()),
            Short('o') | Long("output") if command == "pack" || command == "render" => {
                out = Some(args.value()?.into())
            }
            Long("fonts") if command == "render" => font_dir = Some(args.value()?.into()),
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
    if let Some(dir) = &font_dir
        && !dir.is_dir()
    {
        return Err(Fail::Io(format!("{}: not a folder", dir.display())));
    }
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
    let fonts: Box<dyn unbaked_render::FontSource> = match font_dir {
        Some(dir) => Box::new(fonts::FontFolder::new(dir)),
        None => Box::new(unbaked_render::NoFonts),
    };
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
        let mut report = status_json(&status);
        report["ok"] = true.into();
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
