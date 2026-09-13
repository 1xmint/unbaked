//! Writing packages (SPEC.md section 3.2), and moving them between a carrier
//! and a folder (the directory form, section 3.3).

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{self, Cursor, Write};
use std::path::{Path, PathBuf};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

use crate::open::{Container, OpenError, detect};
use crate::package::{Limits, Package, PackageError};
use crate::{mp4, png, sniff};

/// Files that go into a package: package path -> bytes.
pub type Files = BTreeMap<String, Vec<u8>>;

/// Entries at or above this size are written with ZIP64 sizes.
const LARGE_FILE: u64 = 0xFFFF_0000;

/// Builds a package from `files` and checks the result opens under `limits`.
///
/// The same files always give the same bytes: entries are sorted, timestamps
/// are fixed at 1980-01-01, and permissions are fixed. Images, video, audio and
/// fonts are stored as they are; everything else is deflated. Folders are not
/// written as entries.
pub fn write(files: &Files, limits: Limits) -> Result<Vec<u8>, PackageError> {
    let zip_err = |e: zip::result::ZipError| PackageError::Zip(e.to_string());
    let io_err = |e: io::Error| PackageError::Zip(e.to_string());
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in files {
        crate::package::check_name(name)?;
        if name.ends_with('/') {
            return Err(PackageError::BadName {
                name: name.clone(),
                reason: "is a folder, not a file",
            });
        }
        let head = &data[..data.len().min(sniff::HEADER_LEN)];
        let method = if sniff::detect(head).is_some() {
            CompressionMethod::Stored
        } else {
            CompressionMethod::Deflated
        };
        let options = SimpleFileOptions::default()
            .compression_method(method)
            .last_modified_time(DateTime::DEFAULT)
            .unix_permissions(0o644)
            .large_file(data.len() as u64 >= LARGE_FILE);
        zip.start_file(name.as_str(), options).map_err(zip_err)?;
        zip.write_all(data).map_err(io_err)?;
    }
    let bytes = zip.finish().map_err(zip_err)?.into_inner();
    Package::open(&bytes, limits)?;
    Ok(bytes)
}

/// Returns a copy of `carrier` holding `package` in its hidden slot, replacing
/// any package it already had.
pub fn with_package(carrier: &[u8], package: &[u8]) -> Result<Vec<u8>, OpenError> {
    match detect(carrier).ok_or(OpenError::UnknownFormat)? {
        Container::Png => png::write_slot(carrier, package).map_err(OpenError::Png),
        Container::Mp4 => mp4::write_slot(carrier, package).map_err(OpenError::Mp4),
    }
}

/// Reads every file the spec defines out of a carrier's package, checking
/// sizes and checksums. `x-` entries are left out.
pub fn read_files(carrier: &[u8], limits: Limits) -> Result<Files, OpenError> {
    let slot = match detect(carrier).ok_or(OpenError::UnknownFormat)? {
        Container::Png => png::read_slot(carrier).map_err(OpenError::Png)?,
        Container::Mp4 => mp4::read_slot(carrier).map_err(OpenError::Mp4)?,
    };
    let slot = slot.ok_or(OpenError::NoPackage)?;
    let mut package = Package::open(slot, limits).map_err(OpenError::Package)?;
    let names: Vec<String> = package.file_names().map(str::to_owned).collect();
    let mut files = Files::new();
    for name in names {
        let data = package.read(&name).map_err(OpenError::Package)?;
        files.insert(name, data);
    }
    Ok(files)
}

/// Why a folder could not be written or read.
#[derive(Debug)]
pub enum FolderError {
    Io {
        path: PathBuf,
        error: io::Error,
    },
    /// The target folder already has something in it.
    NotEmpty(PathBuf),
    /// Symbolic links are never followed or packed.
    Symlink(PathBuf),
    /// A file or folder name that is not valid UTF-8.
    NameNotUtf8(PathBuf),
    /// A name that cannot be written safely on every system, such as `CON` or `a.`.
    UnportableName {
        name: String,
        reason: &'static str,
    },
    /// The folder breaks a package rule or limit.
    Package(PackageError),
}

impl fmt::Display for FolderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FolderError::Io { path, error } => write!(f, "{}: {error}", path.display()),
            FolderError::NotEmpty(path) => write!(f, "{} is not empty", path.display()),
            FolderError::Symlink(path) => {
                write!(f, "{} is a symbolic link", path.display())
            }
            FolderError::NameNotUtf8(path) => {
                write!(f, "{} has a name that is not valid UTF-8", path.display())
            }
            FolderError::UnportableName { name, reason } => write!(f, "{name:?} {reason}"),
            FolderError::Package(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for FolderError {}

fn io_at(path: &Path) -> impl FnOnce(io::Error) -> FolderError + '_ {
    move |error| FolderError::Io {
        path: path.to_owned(),
        error,
    }
}

/// Names Windows treats as devices in any folder, with or without an extension.
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Rejects names that would write somewhere else, or collide, on some system.
fn check_portable(name: &str) -> Result<(), FolderError> {
    let bad = |reason| {
        Err(FolderError::UnportableName {
            name: name.to_owned(),
            reason,
        })
    };
    for segment in name.split('/') {
        let stem = segment.split('.').next().unwrap_or("");
        if RESERVED
            .iter()
            .any(|r| r.eq_ignore_ascii_case(stem.trim_end()))
        {
            return bad("uses a name Windows reserves for devices");
        }
        if segment.ends_with('.') || segment.ends_with(' ') {
            return bad("ends with a dot or space, which Windows drops");
        }
        if segment.chars().any(|c| c < ' ' || "<>\"|?*".contains(c)) {
            return bad("contains a character Windows does not allow in file names");
        }
    }
    Ok(())
}

/// Writes `files` into the folder `dir`, which must be empty or not exist yet.
/// Every name is checked before anything is written. Existing files are never
/// overwritten and links are never followed.
pub fn write_folder(files: &Files, dir: &Path) -> Result<(), FolderError> {
    for name in files.keys() {
        crate::package::check_name(name).map_err(FolderError::Package)?;
        check_portable(name)?;
    }
    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(FolderError::Symlink(dir.to_owned()));
        }
        Ok(_) => {
            if fs::read_dir(dir).map_err(io_at(dir))?.next().is_some() {
                return Err(FolderError::NotEmpty(dir.to_owned()));
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(dir).map_err(io_at(dir))?;
        }
        Err(e) => return Err(io_at(dir)(e)),
    }
    for (name, data) in files {
        let path = name.split('/').fold(dir.to_owned(), |p, s| p.join(s));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io_at(parent))?;
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(io_at(&path))?;
        file.write_all(data).map_err(io_at(&path))?;
    }
    Ok(())
}

/// Reads a package in directory form. Symbolic links are refused, not followed.
/// Names, duplicates and limits are checked as for a ZIP package, so a folder
/// that reads here also writes with [`write`]. `bake.json` may be missing.
pub fn read_folder(dir: &Path, limits: Limits) -> Result<Files, FolderError> {
    let mut files = Files::new();
    let mut total = 0u64;
    let mut stack = vec![(dir.to_owned(), String::new())];
    while let Some((folder, prefix)) = stack.pop() {
        let meta = fs::symlink_metadata(&folder).map_err(io_at(&folder))?;
        if meta.file_type().is_symlink() {
            return Err(FolderError::Symlink(folder));
        }
        for entry in fs::read_dir(&folder).map_err(io_at(&folder))? {
            let entry = entry.map_err(io_at(&folder))?;
            let path = entry.path();
            let file_name = entry
                .file_name()
                .into_string()
                .map_err(|_| FolderError::NameNotUtf8(path.clone()))?;
            let name = format!("{prefix}{file_name}");
            let kind = entry.file_type().map_err(io_at(&path))?;
            if kind.is_symlink() {
                return Err(FolderError::Symlink(path));
            } else if kind.is_dir() {
                stack.push((path, format!("{name}/")));
            } else {
                if files.len() >= limits.max_entries {
                    return Err(FolderError::Package(PackageError::TooManyEntries {
                        limit: limits.max_entries,
                    }));
                }
                let size = entry.metadata().map_err(io_at(&path))?.len();
                total = total.saturating_add(size);
                if total > limits.max_total_size {
                    return Err(FolderError::Package(PackageError::TooLarge {
                        limit: limits.max_total_size,
                    }));
                }
                crate::package::check_name(&name).map_err(FolderError::Package)?;
                let data = fs::read(&path).map_err(io_at(&path))?;
                files.insert(name, data);
            }
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open::{Status, open, sha256_hex};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn files(entries: &[(&str, &[u8])]) -> Files {
        entries
            .iter()
            .map(|(n, d)| (n.to_string(), d.to_vec()))
            .collect()
    }

    const PNG_HEAD: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR pretend image";

    fn sample() -> Files {
        files(&[
            ("recipe.json", br#"{"unbaked": 0}"#),
            ("bake.json", b"{}"),
            ("assets/logo.png", PNG_HEAD),
            ("assets/notes.txt", &[b'a'; 500]),
        ])
    }

    /// A fresh, empty scratch folder path under the system temp folder.
    fn scratch() -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "unbaked-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn written_packages_open_and_are_repeatable() {
        let bytes = write(&sample(), Limits::default()).unwrap();
        assert_eq!(bytes, write(&sample(), Limits::default()).unwrap());

        let mut package = Package::open(&bytes, Limits::default()).unwrap();
        package.verify().unwrap();
        assert_eq!(package.read("assets/logo.png").unwrap(), PNG_HEAD);

        let mut archive = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
        let method =
            |archive: &mut zip::ZipArchive<_>, name| archive.by_name(name).unwrap().compression();
        assert_eq!(
            method(&mut archive, "assets/logo.png"),
            CompressionMethod::Stored
        );
        assert_eq!(
            method(&mut archive, "assets/notes.txt"),
            CompressionMethod::Deflated
        );
        assert_eq!(
            method(&mut archive, "recipe.json"),
            CompressionMethod::Deflated
        );
    }

    #[test]
    fn non_ascii_names_carry_the_utf8_flag() {
        let mut input = sample();
        input.insert("assets/caf\u{e9}.txt".into(), b"x".to_vec());
        let bytes = write(&input, Limits::default()).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
        let start = archive
            .by_name("assets/caf\u{e9}.txt")
            .unwrap()
            .header_start() as usize;
        let flags = u16::from_le_bytes([bytes[start + 6], bytes[start + 7]]);
        assert_ne!(flags & 0x0800, 0);
    }

    #[test]
    fn writing_refuses_what_reading_would_reject() {
        let mut bad = sample();
        bad.insert("assets/../x".into(), vec![]);
        assert!(matches!(
            write(&bad, Limits::default()),
            Err(PackageError::BadName { .. })
        ));

        let mut unknown = sample();
        unknown.insert("notes.txt".into(), vec![]);
        assert!(matches!(
            write(&unknown, Limits::default()),
            Err(PackageError::UnknownRootEntry { .. })
        ));

        let mut clash = sample();
        clash.insert("assets/LOGO.png".into(), vec![]);
        assert!(matches!(
            write(&clash, Limits::default()),
            Err(PackageError::DuplicateName { .. })
        ));

        let mut no_bake = sample();
        no_bake.remove("bake.json");
        assert_eq!(
            write(&no_bake, Limits::default()),
            Err(PackageError::Missing { name: "bake.json" })
        );
    }

    #[test]
    fn a_package_moves_between_carrier_and_folder_unchanged() {
        let png = crate::open::tests::png_pixel([1, 2, 3, 255]);
        let package = write(&sample(), Limits::default()).unwrap();
        let carrier = with_package(&png, &package).unwrap();

        let dir = scratch();
        let from_carrier = read_files(&carrier, Limits::default()).unwrap();
        write_folder(&from_carrier, &dir).unwrap();
        assert_eq!(
            fs::read(dir.join("assets").join("logo.png")).unwrap(),
            PNG_HEAD
        );

        let from_folder = read_folder(&dir, Limits::default()).unwrap();
        assert_eq!(from_folder, sample());
        assert_eq!(write(&from_folder, Limits::default()).unwrap(), package);

        assert!(matches!(
            write_folder(&sample(), &dir),
            Err(FolderError::NotEmpty(_))
        ));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn replacing_the_package_keeps_the_render_and_reports_stale() {
        let render = crate::open::tests::png_pixel([5, 5, 5, 255]);
        let recipe = r##"{"unbaked": 0, "output": {"kind": "image", "width": 1, "height": 1},
            "assets": {}, "layers": []}"##;
        let bake = format!(
            r#"{{"unbaked": 0, "renderer": "t", "recipe_sha256": "{}", "assets_sha256": {{}}, "render_sha256": "{}"}}"#,
            sha256_hex(recipe.as_bytes()),
            sha256_hex(&render)
        );
        let mut input = files(&[
            ("recipe.json", recipe.as_bytes()),
            ("bake.json", bake.as_bytes()),
        ]);
        let fresh = with_package(&render, &write(&input, Limits::default()).unwrap()).unwrap();
        assert_eq!(
            open(&fresh, Limits::default()).unwrap().check(),
            Status::Fresh
        );

        input.insert(
            "recipe.json".into(),
            recipe.replace("\"width\": 1", "\"width\": 2").into_bytes(),
        );
        let edited = with_package(&fresh, &write(&input, Limits::default()).unwrap()).unwrap();
        assert_eq!(
            open(&edited, Limits::default()).unwrap().check(),
            Status::Stale(vec![crate::bake::Change::Recipe])
        );
        assert_eq!(png::without_slot(&edited).unwrap(), render);
    }

    #[test]
    fn unportable_names_are_not_written_to_disk() {
        for name in [
            "assets/CON",
            "assets/nul.png",
            "assets/Com1.txt",
            "assets/file.",
            "assets/file ",
            "assets/a?b.png",
            "assets/a\u{1}b",
        ] {
            let mut input = sample();
            input.insert(name.into(), vec![]);
            let dir = scratch();
            assert!(
                matches!(
                    write_folder(&input, &dir),
                    Err(FolderError::UnportableName { .. })
                ),
                "{name:?}"
            );
            assert!(!dir.exists(), "nothing is written before names are checked");
        }
        // Names that only look similar are fine.
        check_portable("assets/console.png").unwrap();
        check_portable("assets/lpt10.txt").unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn folders_with_symbolic_links_are_refused() {
        let dir = scratch();
        write_folder(&sample(), &dir).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.join("assets/passwd")).unwrap();
        assert!(matches!(
            read_folder(&dir, Limits::default()),
            Err(FolderError::Symlink(_))
        ));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn folder_limits_are_enforced() {
        let dir = scratch();
        write_folder(&sample(), &dir).unwrap();
        let tight = Limits {
            max_entries: 3,
            ..Limits::default()
        };
        assert!(matches!(
            read_folder(&dir, tight),
            Err(FolderError::Package(PackageError::TooManyEntries {
                limit: 3
            }))
        ));
        let small = Limits {
            max_total_size: 100,
            ..Limits::default()
        };
        assert!(matches!(
            read_folder(&dir, small),
            Err(FolderError::Package(PackageError::TooLarge { limit: 100 }))
        ));
        fs::remove_dir_all(&dir).unwrap();
    }
}
