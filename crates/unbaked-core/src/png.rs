//! The PNG slot: one `unBK` chunk holding the package (SPEC.md section 2.1).

use std::fmt;

/// The 8 bytes every PNG file starts with.
pub const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

/// Chunk type of the slot: ancillary, private, reserved bit clear, not safe to copy.
pub const SLOT_TYPE: [u8; 4] = *b"unBK";

/// PNG caps a chunk's data length at 2^31 - 1 bytes.
pub const MAX_CHUNK_LEN: usize = (1 << 31) - 1;

/// Why a PNG could not be read or written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PngError {
    /// The file does not start with the PNG signature.
    NotPng,
    /// The first chunk is not `IHDR`.
    MissingIhdr,
    /// The file ends before `IEND`, or a chunk runs past the end of the file.
    Truncated,
    /// A chunk declares a length above 2^31 - 1.
    ChunkTooLong,
    /// The file has more than one `unBK` chunk.
    DuplicateSlot,
    /// The `unBK` chunk's checksum does not match its contents.
    SlotChecksum,
    /// The package is too large for one PNG chunk.
    PackageTooLarge,
}

impl fmt::Display for PngError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PngError::NotPng => "not a PNG file",
            PngError::MissingIhdr => "PNG does not start with an IHDR chunk",
            PngError::Truncated => "PNG is truncated",
            PngError::ChunkTooLong => "PNG chunk length exceeds 2^31 - 1",
            PngError::DuplicateSlot => "PNG has more than one unBK chunk",
            PngError::SlotChecksum => "unBK chunk checksum does not match",
            PngError::PackageTooLarge => "package exceeds the PNG chunk limit of 2^31 - 1 bytes",
        })
    }
}

impl std::error::Error for PngError {}

/// One chunk, as byte ranges into the file.
struct Chunk {
    kind: [u8; 4],
    /// The whole chunk: length, type, data and CRC.
    whole: std::ops::Range<usize>,
    data: std::ops::Range<usize>,
    crc: u32,
}

/// Walks the chunks from `IHDR` to `IEND`, inclusive. Bytes after `IEND` are ignored.
fn chunks(file: &[u8]) -> Result<Vec<Chunk>, PngError> {
    if !file.starts_with(&SIGNATURE) {
        return Err(PngError::NotPng);
    }
    let mut out = Vec::new();
    let mut pos = SIGNATURE.len();
    loop {
        let header = file.get(pos..pos + 8).ok_or(PngError::Truncated)?;
        let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        if len > MAX_CHUNK_LEN {
            return Err(PngError::ChunkTooLong);
        }
        let kind = [header[4], header[5], header[6], header[7]];
        let data = pos + 8..pos + 8 + len;
        let crc_bytes = file
            .get(data.end..data.end + 4)
            .ok_or(PngError::Truncated)?;
        let chunk = Chunk {
            kind,
            whole: pos..data.end + 4,
            data,
            crc: u32::from_be_bytes([crc_bytes[0], crc_bytes[1], crc_bytes[2], crc_bytes[3]]),
        };
        if out.is_empty() && kind != *b"IHDR" {
            return Err(PngError::MissingIhdr);
        }
        pos = chunk.whole.end;
        out.push(chunk);
        if kind == *b"IEND" {
            return Ok(out);
        }
    }
}

/// Returns the package stored in the file's `unBK` chunk, or `None` if it has none.
pub fn read_slot(file: &[u8]) -> Result<Option<&[u8]>, PngError> {
    let mut found = None;
    for chunk in chunks(file)? {
        if chunk.kind != SLOT_TYPE {
            continue;
        }
        if found.is_some() {
            return Err(PngError::DuplicateSlot);
        }
        let data = &file[chunk.data.clone()];
        if crc32(&[&chunk.kind, data]) != chunk.crc {
            return Err(PngError::SlotChecksum);
        }
        found = Some(data);
    }
    Ok(found)
}

/// Returns a copy of the file with `package` in an `unBK` chunk immediately before
/// `IEND`. Any existing `unBK` chunks and any bytes after `IEND` are dropped.
pub fn write_slot(file: &[u8], package: &[u8]) -> Result<Vec<u8>, PngError> {
    if package.len() > MAX_CHUNK_LEN {
        return Err(PngError::PackageTooLarge);
    }
    let mut out = Vec::with_capacity(file.len() + package.len() + 12);
    out.extend_from_slice(&SIGNATURE);
    for chunk in chunks(file)?.iter().filter(|c| c.kind != SLOT_TYPE) {
        if chunk.kind == *b"IEND" {
            out.extend_from_slice(&(package.len() as u32).to_be_bytes());
            out.extend_from_slice(&SLOT_TYPE);
            out.extend_from_slice(package);
            out.extend_from_slice(&crc32(&[&SLOT_TYPE, package]).to_be_bytes());
        }
        out.extend_from_slice(&file[chunk.whole.clone()]);
    }
    Ok(out)
}

/// Returns a copy of the file with every `unBK` chunk and any bytes after `IEND`
/// removed. `bake.json` fingerprints the render in this form (SPEC.md section 7).
pub fn without_slot(file: &[u8]) -> Result<Vec<u8>, PngError> {
    let mut out = Vec::with_capacity(file.len());
    out.extend_from_slice(&SIGNATURE);
    for chunk in chunks(file)?.iter().filter(|c| c.kind != SLOT_TYPE) {
        out.extend_from_slice(&file[chunk.whole.clone()]);
    }
    Ok(out)
}

/// CRC-32 as PNG uses it (ISO 3309, polynomial 0xEDB88320), over several parts.
pub(crate) fn crc32(parts: &[&[u8]]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut table = [0u32; 256];
        let mut n = 0;
        while n < 256 {
            let mut c = n as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
                k += 1;
            }
            table[n] = c;
            n += 1;
        }
        table
    };
    let mut crc = 0xFFFF_FFFFu32;
    for part in parts {
        for &byte in *part {
            crc = TABLE[((crc ^ byte as u32) & 0xFF) as usize] ^ (crc >> 8);
        }
    }
    crc ^ 0xFFFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
        out
    }

    /// A valid 1x1 transparent RGBA PNG, with a tEXt chunk after IDAT.
    fn tiny_png() -> Vec<u8> {
        let ihdr = [0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0];
        // zlib stored block: filter byte 0 and four zero samples, then Adler-32.
        let idat = [
            0x78, 0x01, 0x01, 0x05, 0x00, 0xfa, 0xff, 0, 0, 0, 0, 0, 0x00, 0x05, 0x00, 0x01,
        ];
        let mut png = SIGNATURE.to_vec();
        png.extend(chunk(b"IHDR", &ihdr));
        png.extend(chunk(b"IDAT", &idat));
        png.extend(chunk(b"tEXt", b"Comment\0hi"));
        png.extend(chunk(b"IEND", &[]));
        png
    }

    #[test]
    fn crc_matches_known_iend_checksum() {
        assert_eq!(crc32(&[b"IEND"]), 0xAE42_6082);
    }

    #[test]
    fn plain_png_has_no_slot() {
        assert_eq!(read_slot(&tiny_png()), Ok(None));
    }

    #[test]
    fn written_package_reads_back() {
        let png = tiny_png();
        let file = write_slot(&png, b"PK fake zip").unwrap();
        assert_eq!(read_slot(&file), Ok(Some(&b"PK fake zip"[..])));
    }

    #[test]
    fn slot_goes_immediately_before_iend() {
        let png = tiny_png();
        let file = write_slot(&png, b"zip").unwrap();
        let iend = chunk(b"IEND", &[]);
        let slot = chunk(b"unBK", b"zip");
        assert!(file.ends_with(&[slot, iend.clone()].concat()));
        assert!(file.starts_with(&png[..png.len() - iend.len()]));
    }

    #[test]
    fn writing_again_replaces_the_slot() {
        let once = write_slot(&tiny_png(), b"first").unwrap();
        let twice = write_slot(&once, b"second").unwrap();
        assert_eq!(read_slot(&twice), Ok(Some(&b"second"[..])));
        assert_eq!(twice.windows(4).filter(|w| *w == SLOT_TYPE).count(), 1);
    }

    #[test]
    fn removing_the_slot_restores_the_original() {
        let png = tiny_png();
        let file = write_slot(&png, b"zip").unwrap();
        assert_eq!(without_slot(&file).unwrap(), png);
    }

    #[test]
    fn bytes_after_iend_are_ignored_and_dropped() {
        let mut png = tiny_png();
        png.extend_from_slice(b"trailing junk");
        assert_eq!(read_slot(&png), Ok(None));
        assert_eq!(without_slot(&png).unwrap(), tiny_png());
    }

    #[test]
    fn slot_anywhere_after_ihdr_is_accepted() {
        let png = tiny_png();
        let ihdr_end = SIGNATURE.len() + 25;
        let file = [
            &png[..ihdr_end],
            &chunk(b"unBK", b"early"),
            &png[ihdr_end..],
        ]
        .concat();
        assert_eq!(read_slot(&file), Ok(Some(&b"early"[..])));
    }

    #[test]
    fn two_slots_are_rejected() {
        let png = tiny_png();
        let ihdr_end = SIGNATURE.len() + 25;
        let one = write_slot(&png, b"a").unwrap();
        let file = [&one[..ihdr_end], &chunk(b"unBK", b"b"), &one[ihdr_end..]].concat();
        assert_eq!(read_slot(&file), Err(PngError::DuplicateSlot));
    }

    #[test]
    fn corrupted_slot_is_rejected() {
        let mut file = write_slot(&tiny_png(), b"zip").unwrap();
        let pos = file.windows(4).position(|w| w == SLOT_TYPE).unwrap() + 4;
        file[pos] ^= 0xFF;
        assert_eq!(read_slot(&file), Err(PngError::SlotChecksum));
    }

    #[test]
    fn malformed_files_are_rejected() {
        let png = tiny_png();
        assert_eq!(read_slot(b"GIF89a"), Err(PngError::NotPng));
        assert_eq!(read_slot(&png[..png.len() - 1]), Err(PngError::Truncated));
        assert_eq!(read_slot(&SIGNATURE), Err(PngError::Truncated));

        let no_ihdr = [&SIGNATURE[..], &chunk(b"IEND", &[])].concat();
        assert_eq!(read_slot(&no_ihdr), Err(PngError::MissingIhdr));

        let mut huge = png.clone();
        huge[8..12].copy_from_slice(&0x8000_0000u32.to_be_bytes());
        assert_eq!(read_slot(&huge), Err(PngError::ChunkTooLong));
    }
}
