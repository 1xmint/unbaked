//! The MP4 / M4A slot: one top-level `uuid` box holding the package (SPEC.md section 2.2).

use std::fmt;

/// Extended type of the slot box: a7094a3c-3a0b-494c-ba92-0dd1b181e4e0.
pub const SLOT_UUID: [u8; 16] = [
    0xa7, 0x09, 0x4a, 0x3c, 0x3a, 0x0b, 0x49, 0x4c, 0xba, 0x92, 0x0d, 0xd1, 0xb1, 0x81, 0xe4, 0xe0,
];

/// Why an MP4 could not be read or written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mp4Error {
    /// The file does not start with an `ftyp` box.
    NotMp4,
    /// A box declares a size smaller than its own header, or runs past the end of the file.
    Truncated,
    /// The file has more than one slot box.
    DuplicateSlot,
    /// The last box runs to the end of the file (size 0) and is too large to be
    /// given an explicit size without moving media data.
    OpenEndedBoxTooLarge,
}

impl fmt::Display for Mp4Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Mp4Error::NotMp4 => "not an MP4 file: no ftyp box at the start",
            Mp4Error::Truncated => "MP4 is truncated or has an invalid box size",
            Mp4Error::DuplicateSlot => "MP4 has more than one Unbaked uuid box",
            Mp4Error::OpenEndedBoxTooLarge => {
                "MP4 ends with a size-0 box over 4 GiB; cannot append after it"
            }
        })
    }
}

impl std::error::Error for Mp4Error {}

/// One top-level box, as byte ranges into the file.
struct TopBox {
    whole: std::ops::Range<usize>,
    /// Everything after the header (and after the extended type, for `uuid`).
    payload: std::ops::Range<usize>,
    extended: Option<[u8; 16]>,
    /// The box declared size 0: it runs to the end of the file.
    open_ended: bool,
}

impl TopBox {
    fn is_slot(&self) -> bool {
        self.extended == Some(SLOT_UUID)
    }
}

fn be_u32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// Walks the top-level boxes of the whole file.
fn boxes(file: &[u8]) -> Result<Vec<TopBox>, Mp4Error> {
    if file.get(4..8) != Some(&b"ftyp"[..]) {
        return Err(Mp4Error::NotMp4);
    }
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < file.len() {
        let header = file.get(pos..pos + 8).ok_or(Mp4Error::Truncated)?;
        let kind = [header[4], header[5], header[6], header[7]];
        let (size, mut header_len, open_ended) = match be_u32(header) {
            0 => (file.len() - pos, 8, true),
            1 => {
                let large = file.get(pos + 8..pos + 16).ok_or(Mp4Error::Truncated)?;
                let large = u64::from_be_bytes(large.try_into().expect("8 bytes"));
                let size = usize::try_from(large).map_err(|_| Mp4Error::Truncated)?;
                (size, 16, false)
            }
            n => (n as usize, 8, false),
        };
        let mut extended = None;
        if kind == *b"uuid" {
            let ext = file
                .get(pos + header_len..pos + header_len + 16)
                .ok_or(Mp4Error::Truncated)?;
            extended = Some(ext.try_into().expect("16 bytes"));
            header_len += 16;
        }
        let end = pos.checked_add(size).ok_or(Mp4Error::Truncated)?;
        if size < header_len || end > file.len() {
            return Err(Mp4Error::Truncated);
        }
        out.push(TopBox {
            whole: pos..end,
            payload: pos + header_len..end,
            extended,
            open_ended,
        });
        pos = end;
    }
    Ok(out)
}

/// Returns the package stored in the file's slot box, or `None` if it has none.
pub fn read_slot(file: &[u8]) -> Result<Option<&[u8]>, Mp4Error> {
    let mut found = None;
    for b in boxes(file)?.iter().filter(|b| b.is_slot()) {
        if found.is_some() {
            return Err(Mp4Error::DuplicateSlot);
        }
        found = Some(&file[b.payload.clone()]);
    }
    Ok(found)
}

/// Returns a copy of the file with `package` in a slot box appended as the last
/// top-level box.
///
/// No media data moves, so sample offsets stay valid. An existing slot that is
/// the last box is removed. An existing slot elsewhere is renamed to a `free` box
/// of the same size instead, because removing it would move whatever follows. A
/// size-0 last box is given its explicit size so the new box is not swallowed.
pub fn write_slot(file: &[u8], package: &[u8]) -> Result<Vec<u8>, Mp4Error> {
    let boxes = boxes(file)?;
    let keep_until = match boxes.last() {
        Some(last) if last.is_slot() => last.whole.start,
        _ => file.len(),
    };

    let mut out = Vec::with_capacity(keep_until + package.len() + 32);
    out.extend_from_slice(&file[..keep_until]);
    for b in boxes.iter().filter(|b| b.whole.end <= keep_until) {
        if b.is_slot() {
            out[b.whole.start + 4..b.whole.start + 8].copy_from_slice(b"free");
        }
        if b.open_ended {
            let size = u32::try_from(b.whole.len()).map_err(|_| Mp4Error::OpenEndedBoxTooLarge)?;
            out[b.whole.start..b.whole.start + 4].copy_from_slice(&size.to_be_bytes());
        }
    }

    let total = 8 + 16 + package.len();
    match u32::try_from(total) {
        Ok(size) => out.extend_from_slice(&size.to_be_bytes()),
        Err(_) => {
            out.extend_from_slice(&1u32.to_be_bytes());
            out.extend_from_slice(b"uuid");
            out.extend_from_slice(&(total as u64 + 8).to_be_bytes());
            out.extend_from_slice(&SLOT_UUID);
            out.extend_from_slice(package);
            return Ok(out);
        }
    }
    out.extend_from_slice(b"uuid");
    out.extend_from_slice(&SLOT_UUID);
    out.extend_from_slice(package);
    Ok(out)
}

/// Returns a copy of the file with every slot box removed. `bake.json`
/// fingerprints the render in this form (SPEC.md section 7). The result is for
/// hashing: if a slot was not the last box, removing it may break playback.
pub fn without_slot(file: &[u8]) -> Result<Vec<u8>, Mp4Error> {
    let mut out = Vec::with_capacity(file.len());
    for b in boxes(file)?.iter().filter(|b| !b.is_slot()) {
        out.extend_from_slice(&file[b.whole.clone()]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mp4_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = ((8 + payload.len()) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    }

    fn slot_box(package: &[u8]) -> Vec<u8> {
        let mut out = ((24 + package.len()) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(b"uuid");
        out.extend_from_slice(&SLOT_UUID);
        out.extend_from_slice(package);
        out
    }

    /// Box layout of a typical MP4. Contents are placeholders.
    fn tiny_mp4() -> Vec<u8> {
        [
            mp4_box(b"ftyp", b"isom\0\0\x02\0isomiso2mp41"),
            mp4_box(b"moov", b"placeholder"),
            mp4_box(b"mdat", b"media bytes"),
        ]
        .concat()
    }

    #[test]
    fn plain_mp4_has_no_slot() {
        assert_eq!(read_slot(&tiny_mp4()), Ok(None));
    }

    #[test]
    fn written_package_reads_back_and_is_appended() {
        let mp4 = tiny_mp4();
        let file = write_slot(&mp4, b"PK fake zip").unwrap();
        assert_eq!(read_slot(&file), Ok(Some(&b"PK fake zip"[..])));
        assert!(file.starts_with(&mp4));
        assert!(file.ends_with(&slot_box(b"PK fake zip")));
    }

    #[test]
    fn writing_again_replaces_a_last_slot() {
        let once = write_slot(&tiny_mp4(), b"first").unwrap();
        let twice = write_slot(&once, b"second").unwrap();
        assert_eq!(twice, [tiny_mp4(), slot_box(b"second")].concat());
    }

    #[test]
    fn removing_the_slot_restores_the_original() {
        let mp4 = tiny_mp4();
        assert_eq!(
            without_slot(&write_slot(&mp4, b"zip").unwrap()).unwrap(),
            mp4
        );
    }

    #[test]
    fn other_uuid_boxes_are_not_slots() {
        let mut other = 32u32.to_be_bytes().to_vec();
        other.extend_from_slice(b"uuid");
        other.extend_from_slice(&[7; 16]);
        other.extend_from_slice(b"not ours");
        let file = [tiny_mp4(), other].concat();
        assert_eq!(read_slot(&file), Ok(None));
        assert_eq!(without_slot(&file).unwrap(), file);
    }

    #[test]
    fn slot_anywhere_is_accepted() {
        let mp4 = tiny_mp4();
        let ftyp_len = 28;
        let file = [&mp4[..ftyp_len], &slot_box(b"early"), &mp4[ftyp_len..]].concat();
        assert_eq!(read_slot(&file), Ok(Some(&b"early"[..])));
    }

    #[test]
    fn a_middle_slot_becomes_free_space_so_media_does_not_move() {
        let mp4 = tiny_mp4();
        let ftyp_len = 28;
        let file = [&mp4[..ftyp_len], &slot_box(b"early"), &mp4[ftyp_len..]].concat();
        let rewritten = write_slot(&file, b"new").unwrap();
        assert_eq!(read_slot(&rewritten), Ok(Some(&b"new"[..])));
        assert_eq!(&rewritten[ftyp_len + 4..ftyp_len + 8], b"free");
        assert_eq!(rewritten.len(), file.len() + slot_box(b"new").len());
        assert_eq!(
            &rewritten[..file.len()][ftyp_len + 8..],
            &file[ftyp_len + 8..]
        );
    }

    #[test]
    fn two_slots_are_rejected() {
        let file = [tiny_mp4(), slot_box(b"a"), slot_box(b"b")].concat();
        assert_eq!(read_slot(&file), Err(Mp4Error::DuplicateSlot));
    }

    #[test]
    fn size_zero_last_box_gets_an_explicit_size() {
        let mut mp4 = tiny_mp4();
        let mdat_start = mp4.len() - 19;
        mp4[mdat_start..mdat_start + 4].copy_from_slice(&0u32.to_be_bytes());
        assert_eq!(read_slot(&mp4), Ok(None));
        let file = write_slot(&mp4, b"zip").unwrap();
        assert_eq!(read_slot(&file), Ok(Some(&b"zip"[..])));
        assert_eq!(file, [tiny_mp4(), slot_box(b"zip")].concat());
    }

    #[test]
    fn largesize_boxes_are_read() {
        let mut large = 1u32.to_be_bytes().to_vec();
        large.extend_from_slice(b"uuid");
        large.extend_from_slice(&(16u64 + 16 + 3).to_be_bytes());
        large.extend_from_slice(&SLOT_UUID);
        large.extend_from_slice(b"zip");
        let file = [tiny_mp4(), large].concat();
        assert_eq!(read_slot(&file), Ok(Some(&b"zip"[..])));
    }

    #[test]
    fn malformed_files_are_rejected() {
        let mp4 = tiny_mp4();
        assert_eq!(read_slot(b"\x89PNG\r\n\x1a\n"), Err(Mp4Error::NotMp4));
        assert_eq!(read_slot(&mp4[..mp4.len() - 1]), Err(Mp4Error::Truncated));
        assert_eq!(
            read_slot(&[mp4.clone(), vec![0, 0, 0]].concat()),
            Err(Mp4Error::Truncated)
        );

        let mut tiny_size = mp4.clone();
        tiny_size[28..32].copy_from_slice(&4u32.to_be_bytes());
        assert_eq!(read_slot(&tiny_size), Err(Mp4Error::Truncated));

        let cut_uuid = [mp4.clone(), slot_box(b"zip")[..20].to_vec()].concat();
        assert_eq!(read_slot(&cut_uuid), Err(Mp4Error::Truncated));

        let mut huge_large = 1u32.to_be_bytes().to_vec();
        huge_large.extend_from_slice(b"free");
        huge_large.extend_from_slice(&u64::MAX.to_be_bytes());
        assert_eq!(
            read_slot(&[mp4, huge_large].concat()),
            Err(Mp4Error::Truncated)
        );
    }
}
