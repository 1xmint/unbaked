//! The package: a ZIP archive checked against SPEC.md sections 3.1 and 3.2.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::io::{Cursor, Read};

use unicase::UniCase;
use unicode_normalization::UnicodeNormalization;
use zip::{CompressionMethod, ZipArchive};

/// Size limits applied while opening and reading a package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Most entries the archive may list.
    pub max_entries: usize,
    /// Most bytes all entries may decompress to, together.
    pub max_total_size: u64,
    /// Highest decompressed-to-compressed ratio for a deflated entry.
    pub max_ratio: u64,
}

impl Default for Limits {
    /// The suggested defaults: 10,000 entries, 4 GiB total, 100:1 ratio.
    fn default() -> Self {
        Limits {
            max_entries: 10_000,
            max_total_size: 4 << 30,
            max_ratio: 100,
        }
    }
}

/// Why a package was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageError {
    /// The ZIP structure itself is invalid, or a checksum failed.
    Zip(String),
    /// An entry name is not valid UTF-8.
    NameNotUtf8,
    /// An entry name breaks a naming rule.
    BadName {
        name: String,
        reason: &'static str,
    },
    /// Two entries name the same file once normalised and case folded.
    DuplicateName {
        first: String,
        second: String,
    },
    /// A file's path is also used as a folder by another entry.
    FileAndFolder {
        name: String,
    },
    /// A root entry this spec version does not define.
    UnknownRootEntry {
        name: String,
    },
    /// `recipe.json` or `bake.json` is missing.
    Missing {
        name: &'static str,
    },
    Encrypted {
        name: String,
    },
    UnsupportedMethod {
        name: String,
        method: String,
    },
    Symlink {
        name: String,
    },
    /// Two entries' bytes overlap inside the archive.
    Overlap {
        name: String,
    },
    TooManyEntries {
        limit: usize,
    },
    TooLarge {
        limit: u64,
    },
    RatioTooHigh {
        name: String,
        limit: u64,
    },
    /// An entry decompressed to a different size than it declared.
    SizeMismatch {
        name: String,
    },
    /// No file with this name in the package.
    NotFound {
        name: String,
    },
}

impl fmt::Display for PackageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use PackageError::*;
        match self {
            Zip(e) => write!(f, "invalid ZIP archive: {e}"),
            NameNotUtf8 => write!(f, "entry name is not valid UTF-8"),
            BadName { name, reason } => write!(f, "entry name {name:?} {reason}"),
            DuplicateName { first, second } => {
                write!(f, "entries {first:?} and {second:?} name the same file")
            }
            FileAndFolder { name } => write!(f, "{name:?} is used as both a file and a folder"),
            UnknownRootEntry { name } => write!(f, "unknown root entry {name:?}"),
            Missing { name } => write!(f, "package has no {name}"),
            Encrypted { name } => write!(f, "entry {name:?} is encrypted"),
            UnsupportedMethod { name, method } => {
                write!(f, "entry {name:?} uses unsupported compression {method}")
            }
            Symlink { name } => write!(f, "entry {name:?} is a symbolic link"),
            Overlap { name } => write!(f, "entry {name:?} overlaps another entry"),
            TooManyEntries { limit } => write!(f, "package has more than {limit} entries"),
            TooLarge { limit } => write!(f, "package decompresses to more than {limit} bytes"),
            RatioTooHigh { name, limit } => {
                write!(f, "entry {name:?} expands more than {limit}:1")
            }
            SizeMismatch { name } => write!(f, "entry {name:?} does not match its declared size"),
            NotFound { name } => write!(f, "package has no file {name:?}"),
        }
    }
}

impl std::error::Error for PackageError {}

fn zip_err(e: impl fmt::Display) -> PackageError {
    PackageError::Zip(e.to_string())
}

/// A checked package, borrowed from the bytes of a carrier's slot.
pub struct Package<'a> {
    archive: ZipArchive<Cursor<&'a [u8]>>,
    /// File name -> (entry index, declared size). Folders and `x-` entries are not listed.
    files: BTreeMap<String, (usize, u64)>,
}

impl<'a> Package<'a> {
    /// Opens a package and checks everything that can be checked without
    /// decompressing: names, duplicates, root entries, methods, encryption,
    /// symbolic links, overlaps and limits. Sizes and checksums are checked as
    /// files are read; [`Package::verify`] reads them all.
    pub fn open(bytes: &'a [u8], limits: Limits) -> Result<Self, PackageError> {
        let mut archive = ZipArchive::new(Cursor::new(bytes)).map_err(zip_err)?;
        if archive.len() > limits.max_entries {
            return Err(PackageError::TooManyEntries {
                limit: limits.max_entries,
            });
        }

        let mut files = BTreeMap::new();
        let mut seen: HashMap<UniCase<String>, String> = HashMap::new();
        let mut file_keys = Vec::new();
        let mut spans = Vec::new();
        let mut total = 0u64;

        for index in 0..archive.len() {
            let entry = archive.by_index_raw(index).map_err(zip_err)?;
            let name = std::str::from_utf8(entry.name_raw())
                .map_err(|_| PackageError::NameNotUtf8)?
                .to_owned();
            check_name(&name)?;
            let is_dir = name.ends_with('/');
            let path = name.trim_end_matches('/');

            let key = UniCase::new(path.nfc().collect::<String>());
            if let Some(first) = seen.insert(key.clone(), name.clone()) {
                return Err(PackageError::DuplicateName {
                    first,
                    second: name,
                });
            }
            if !is_dir {
                file_keys.push(key);
            }

            if entry.encrypted() {
                return Err(PackageError::Encrypted { name });
            }
            match entry.compression() {
                CompressionMethod::Stored | CompressionMethod::Deflated => {}
                other => {
                    return Err(PackageError::UnsupportedMethod {
                        name,
                        method: format!("{other:?}"),
                    });
                }
            }
            if entry.is_symlink() {
                return Err(PackageError::Symlink { name });
            }

            let size = entry.size();
            total = total.saturating_add(size);
            if total > limits.max_total_size {
                return Err(PackageError::TooLarge {
                    limit: limits.max_total_size,
                });
            }
            if entry.compression() == CompressionMethod::Deflated
                && size > entry.compressed_size().saturating_mul(limits.max_ratio)
            {
                return Err(PackageError::RatioTooHigh {
                    name,
                    limit: limits.max_ratio,
                });
            }

            let data_start = entry
                .data_start()
                .ok_or_else(|| zip_err("no data offset"))?;
            let end = data_start.saturating_add(entry.compressed_size());
            spans.push((entry.header_start(), end, name.clone()));

            match root_entry(path) {
                Root::Ignored => {}
                Root::Unknown => return Err(PackageError::UnknownRootEntry { name }),
                Root::Defined if is_dir => {
                    if path != "assets" && !path.starts_with("assets/") {
                        return Err(PackageError::UnknownRootEntry { name });
                    }
                }
                Root::Defined => {
                    files.insert(name, (index, size));
                }
            }
        }

        spans.sort();
        for pair in spans.windows(2) {
            if pair[1].0 < pair[0].1 {
                return Err(PackageError::Overlap {
                    name: pair[1].2.clone(),
                });
            }
        }

        let file_set: HashSet<&UniCase<String>> = file_keys.iter().collect();
        for key in &file_keys {
            let mut folder: &str = key;
            while let Some((parent, _)) = folder.rsplit_once('/') {
                if file_set.contains(&UniCase::new(parent.to_owned())) {
                    return Err(PackageError::FileAndFolder {
                        name: parent.to_owned(),
                    });
                }
                folder = parent;
            }
        }

        for required in ["recipe.json", "bake.json"] {
            if !files.contains_key(required) {
                return Err(PackageError::Missing { name: required });
            }
        }

        Ok(Package { archive, files })
    }

    /// Names of the files the spec defines (`recipe.json`, `bake.json`, `assets/…`),
    /// in byte order. Folders and ignored `x-` entries are not listed.
    pub fn file_names(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }

    /// Decompresses one file, checking its size and checksum.
    pub fn read(&mut self, name: &str) -> Result<Vec<u8>, PackageError> {
        let &(index, size) = self.files.get(name).ok_or_else(|| PackageError::NotFound {
            name: name.to_owned(),
        })?;
        let mismatch = || PackageError::SizeMismatch {
            name: name.to_owned(),
        };
        let entry = self.archive.by_index(index).map_err(zip_err)?;
        let capacity = usize::try_from(size).map_err(|_| mismatch())?;
        let mut out = Vec::with_capacity(capacity.min(64 << 20));
        entry
            .take(size.saturating_add(1))
            .read_to_end(&mut out)
            .map_err(zip_err)?;
        if out.len() as u64 != size {
            return Err(mismatch());
        }
        Ok(out)
    }

    /// Reads every listed file, so every size and checksum is checked.
    pub fn verify(&mut self) -> Result<(), PackageError> {
        let names: Vec<String> = self.files.keys().cloned().collect();
        for name in names {
            self.read(&name)?;
        }
        Ok(())
    }
}

enum Root {
    /// `recipe.json`, `bake.json` or under `assets/`.
    Defined,
    /// Starts with `x-`.
    Ignored,
    Unknown,
}

fn root_entry(path: &str) -> Root {
    let first = path.split('/').next().unwrap_or("");
    match first {
        "recipe.json" | "bake.json" if first == path => Root::Defined,
        "assets" => Root::Defined,
        _ if first.starts_with("x-") => Root::Ignored,
        _ => Root::Unknown,
    }
}

/// Checks the naming rules of SPEC.md section 3.2.
pub(crate) fn check_name(name: &str) -> Result<(), PackageError> {
    let bad = |reason| {
        Err(PackageError::BadName {
            name: name.to_owned(),
            reason,
        })
    };
    if name.is_empty() {
        return bad("is empty");
    }
    if name.starts_with('/') {
        return bad("starts with /");
    }
    if name.contains('\\') {
        return bad("contains \\");
    }
    if name.contains('\0') {
        return bad("contains NUL");
    }
    if name.contains(':') {
        return bad("contains :");
    }
    for segment in name.strip_suffix('/').unwrap_or(name).split('/') {
        match segment {
            "" => return bad("has an empty path segment"),
            "." | ".." => return bad("has a . or .. segment"),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    /// One entry for the raw builder: stored data with full control of the headers.
    struct Raw<'a> {
        name: &'a [u8],
        data: &'a [u8],
        flags: u16,
        method: u16,
        declared_size: Option<u32>,
        unix_mode: u32,
        offset_override: Option<u32>,
    }

    fn raw<'a>(name: &'a str, data: &'a [u8]) -> Raw<'a> {
        Raw {
            name: name.as_bytes(),
            data,
            flags: 0x0800,
            method: 0,
            declared_size: None,
            unix_mode: 0o100644,
            offset_override: None,
        }
    }

    /// Builds a ZIP by hand so tests can create archives no normal writer would.
    fn build(entries: &[Raw]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for e in entries {
            let offset = out.len() as u32;
            let crc = crate::png::crc32(&[e.data]);
            let size = e.declared_size.unwrap_or(e.data.len() as u32);
            let common = |v: &mut Vec<u8>| {
                v.extend_from_slice(&20u16.to_le_bytes());
                v.extend_from_slice(&e.flags.to_le_bytes());
                v.extend_from_slice(&e.method.to_le_bytes());
                v.extend_from_slice(&[0, 0, 0x21, 0]);
                v.extend_from_slice(&crc.to_le_bytes());
                v.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
                v.extend_from_slice(&size.to_le_bytes());
                v.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
                v.extend_from_slice(&0u16.to_le_bytes());
            };
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            common(&mut out);
            out.extend_from_slice(e.name);
            out.extend_from_slice(e.data);

            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&0x031Eu16.to_le_bytes());
            common(&mut central);
            central.extend_from_slice(&[0; 6]);
            central.extend_from_slice(&(e.unix_mode << 16).to_le_bytes());
            central.extend_from_slice(&e.offset_override.unwrap_or(offset).to_le_bytes());
            central.extend_from_slice(e.name);
        }
        let cd_offset = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    fn minimal() -> Vec<Raw<'static>> {
        vec![raw("recipe.json", b"{}"), raw("bake.json", b"{}")]
    }

    fn with(extra: Raw<'static>) -> Vec<u8> {
        let mut entries = minimal();
        entries.push(extra);
        build(&entries)
    }

    fn open(bytes: &[u8]) -> Result<Package<'_>, PackageError> {
        Package::open(bytes, Limits::default())
    }

    fn open_err(bytes: &[u8]) -> PackageError {
        open(bytes).err().expect("package should be rejected")
    }

    #[test]
    fn valid_package_opens_and_reads() {
        let bytes = build(&[
            raw("recipe.json", b"{\"unbaked\":0}"),
            raw("bake.json", b"{}"),
            raw("assets/", b""),
            raw("assets/logo.png", b"png bytes"),
            raw("x-editor/state.bin", b"ignored"),
        ]);
        let mut package = open(&bytes).unwrap();
        assert_eq!(
            package.file_names().collect::<Vec<_>>(),
            ["assets/logo.png", "bake.json", "recipe.json"]
        );
        assert_eq!(package.read("recipe.json").unwrap(), b"{\"unbaked\":0}");
        assert_eq!(package.read("assets/logo.png").unwrap(), b"png bytes");
        assert!(matches!(
            package.read("x-editor/state.bin"),
            Err(PackageError::NotFound { .. })
        ));
        package.verify().unwrap();
    }

    #[test]
    fn deflated_entries_written_by_a_normal_writer_read_back() {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let deflate = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        zip.start_file("recipe.json", deflate).unwrap();
        zip.write_all(br#"{"unbaked": 0, "layers": []}"#).unwrap();
        zip.start_file("bake.json", deflate).unwrap();
        zip.write_all(b"{}").unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        let mut package = open(&bytes).unwrap();
        assert_eq!(
            package.read("recipe.json").unwrap(),
            br#"{"unbaked": 0, "layers": []}"#
        );
    }

    #[test]
    fn required_files_must_exist() {
        let only_recipe = build(&[raw("recipe.json", b"{}")]);
        assert_eq!(
            open_err(&only_recipe),
            PackageError::Missing { name: "bake.json" }
        );
    }

    #[test]
    fn bad_names_are_rejected() {
        for name in [
            "/etc/passwd",
            "assets\\logo.png",
            "assets/../recipe.json",
            "assets/./logo.png",
            "C:/Windows/file",
            "assets/file.txt:stream",
            "assets//logo.png",
            "assets/nul\0byte",
        ] {
            assert!(
                matches!(
                    open_err(&with(raw(name, b"x"))),
                    PackageError::BadName { .. }
                ),
                "{name:?} should be a bad name"
            );
        }
    }

    #[test]
    fn non_utf8_names_are_rejected() {
        let mut entry = raw("assets/x", b"x");
        entry.name = b"assets/\xff";
        assert_eq!(open_err(&with(entry)), PackageError::NameNotUtf8);
    }

    #[test]
    fn names_equal_after_case_folding_or_nfc_are_duplicates() {
        let case = with(raw("RECIPE.JSON", b"{}"));
        assert!(matches!(
            open_err(&case),
            PackageError::DuplicateName { .. }
        ));

        let composed = "assets/caf\u{e9}.png";
        let decomposed = "assets/cafe\u{301}.png";
        let mut entries = minimal();
        entries.push(raw(composed, b"a"));
        entries.push(raw(decomposed, b"b"));
        assert!(matches!(
            open_err(&build(&entries)),
            PackageError::DuplicateName { .. }
        ));

        let mut entries = minimal();
        entries.push(raw("assets/Stra\u{df}e.png", b"a"));
        entries.push(raw("assets/STRASSE.png", b"b"));
        assert!(matches!(
            open_err(&build(&entries)),
            PackageError::DuplicateName { .. }
        ));
    }

    #[test]
    fn a_path_cannot_be_both_file_and_folder() {
        let mut entries = minimal();
        entries.push(raw("assets/fonts", b"a file"));
        entries.push(raw("assets/Fonts/Inter.ttf", b"font"));
        assert!(matches!(
            open_err(&build(&entries)),
            PackageError::FileAndFolder { .. }
        ));
    }

    #[test]
    fn unknown_root_entries_are_rejected_unless_x_prefixed() {
        assert_eq!(
            open_err(&with(raw("notes.txt", b"x"))),
            PackageError::UnknownRootEntry {
                name: "notes.txt".into()
            }
        );
        assert!(matches!(
            open_err(&with(raw("recipe.json/inner", b"x"))),
            PackageError::UnknownRootEntry { .. }
        ));
        open(&with(raw("x-notes.txt", b"x"))).unwrap();
    }

    #[test]
    fn encryption_methods_and_symlinks_are_rejected() {
        let mut encrypted = raw("assets/a", b"x");
        encrypted.flags |= 1;
        assert!(matches!(
            open_err(&with(encrypted)),
            PackageError::Encrypted { .. }
        ));

        let mut bzip = raw("assets/a", b"x");
        bzip.method = 12;
        assert!(matches!(
            open_err(&with(bzip)),
            PackageError::UnsupportedMethod { .. }
        ));

        let mut link = raw("assets/a", b"../../etc/passwd");
        link.unix_mode = 0o120777;
        assert_eq!(
            open_err(&with(link)),
            PackageError::Symlink {
                name: "assets/a".into()
            }
        );
    }

    #[test]
    fn overlapping_entries_are_rejected() {
        let mut entries = minimal();
        let mut second = raw("assets/b", b"{}");
        second.offset_override = Some(0);
        entries.push(second);
        assert!(matches!(
            open_err(&build(&entries)),
            PackageError::Overlap { .. }
        ));
    }

    #[test]
    fn declared_size_must_match() {
        let mut lying = raw("assets/a", b"four");
        lying.declared_size = Some(3);
        let bytes = with(lying);
        assert_eq!(
            open(&bytes).and_then(|mut p| p.verify()).err(),
            Some(PackageError::SizeMismatch {
                name: "assets/a".into()
            })
        );
    }

    #[test]
    fn corrupted_data_fails_the_checksum() {
        let mut bytes = with(raw("assets/a", b"hello"));
        let pos = bytes.windows(5).position(|w| w == b"hello").unwrap();
        bytes[pos] = b'j';
        let mut package = open(&bytes).unwrap();
        assert!(matches!(
            package.read("assets/a"),
            Err(PackageError::Zip(_))
        ));
    }

    #[test]
    fn limits_are_enforced() {
        let bytes = with(raw("assets/a", &[7; 1000]));
        let tight = |f: fn(&mut Limits)| {
            let mut limits = Limits::default();
            f(&mut limits);
            Package::open(&bytes, limits).err()
        };
        assert_eq!(
            tight(|l| l.max_entries = 2),
            Some(PackageError::TooManyEntries { limit: 2 })
        );
        assert_eq!(
            tight(|l| l.max_total_size = 500),
            Some(PackageError::TooLarge { limit: 500 })
        );

        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let deflate = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for name in ["recipe.json", "bake.json"] {
            zip.start_file(name, deflate).unwrap();
            zip.write_all(b"{}").unwrap();
        }
        zip.start_file("assets/zeros.bin", deflate).unwrap();
        zip.write_all(&vec![0; 1 << 20]).unwrap();
        let bomb = zip.finish().unwrap().into_inner();
        assert!(matches!(
            open_err(&bomb),
            PackageError::RatioTooHigh { limit: 100, .. }
        ));
    }

    #[test]
    fn not_a_zip_is_rejected() {
        assert!(matches!(open_err(b"not a zip"), PackageError::Zip(_)));
    }
}
