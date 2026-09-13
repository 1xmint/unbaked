//! Tells an asset's kind from its first bytes (SPEC.md section 4.3). Only the
//! header is read; whether the file really decodes is for the renderer to find out.

/// What an asset file looks like from its header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    Png,
    Jpeg,
    /// An MP4 or M4A. Which tracks it holds is not checked here.
    Mp4,
    Mp3,
    Wav,
    Flac,
    /// TrueType, OpenType, or a font collection.
    Font,
}

impl AssetKind {
    pub fn is_image(self) -> bool {
        matches!(self, AssetKind::Png | AssetKind::Jpeg)
    }

    pub fn is_video(self) -> bool {
        self == AssetKind::Mp4
    }

    /// Audio clips may use audio files or the sound of a video file.
    pub fn is_audio(self) -> bool {
        matches!(
            self,
            AssetKind::Mp4 | AssetKind::Mp3 | AssetKind::Wav | AssetKind::Flac
        )
    }

    pub fn is_font(self) -> bool {
        self == AssetKind::Font
    }
}

/// Bytes [`detect`] needs at most.
pub const HEADER_LEN: usize = 12;

/// The kind of a file from its first [`HEADER_LEN`] bytes, or `None` if it is none of the supported formats.
pub fn detect(head: &[u8]) -> Option<AssetKind> {
    let at = |start: usize, magic: &[u8]| head.get(start..start + magic.len()) == Some(magic);
    if at(0, &crate::png::SIGNATURE) {
        return Some(AssetKind::Png);
    }
    if at(0, &[0xFF, 0xD8, 0xFF]) {
        return Some(AssetKind::Jpeg);
    }
    if at(4, b"ftyp") {
        return Some(AssetKind::Mp4);
    }
    if at(0, b"RIFF") && at(8, b"WAVE") {
        return Some(AssetKind::Wav);
    }
    if at(0, b"fLaC") {
        return Some(AssetKind::Flac);
    }
    if at(0, &[0, 1, 0, 0]) || at(0, b"true") || at(0, b"OTTO") || at(0, b"ttcf") {
        return Some(AssetKind::Font);
    }
    if at(0, b"ID3") || is_mp3_frame(head) {
        return Some(AssetKind::Mp3);
    }
    None
}

/// An MPEG audio Layer III frame header: 11 sync bits, a real version, layer bits `01`.
fn is_mp3_frame(head: &[u8]) -> bool {
    match head {
        [0xFF, b, c, ..] => {
            let sync = b & 0xE0 == 0xE0;
            let version_ok = (b >> 3) & 0b11 != 0b01;
            let layer3 = (b >> 1) & 0b11 == 0b01;
            let bitrate_ok = c >> 4 != 0b1111;
            let rate_ok = (c >> 2) & 0b11 != 0b11;
            sync && version_ok && layer3 && bitrate_ok && rate_ok
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_headers_are_detected() {
        let cases: &[(&[u8], AssetKind)] = &[
            (b"\x89PNG\r\n\x1a\n\0\0\0\x0d", AssetKind::Png),
            (b"\xff\xd8\xff\xe0\0\x10JFIF", AssetKind::Jpeg),
            (b"\0\0\0\x20ftypisom", AssetKind::Mp4),
            (b"\0\0\0\x1cftypM4A ", AssetKind::Mp4),
            (b"RIFF\x24\0\0\0WAVE", AssetKind::Wav),
            (b"fLaC\0\0\0\x22", AssetKind::Flac),
            (b"\0\x01\0\0\0\x10\x01\0", AssetKind::Font),
            (b"OTTO\0\x0c\0\x80", AssetKind::Font),
            (b"ttcf\0\x01\0\0", AssetKind::Font),
            (b"ID3\x04\0\0\0\0", AssetKind::Mp3),
            (b"\xff\xfb\x90\x64", AssetKind::Mp3),
        ];
        for (head, kind) in cases {
            assert_eq!(detect(head), Some(*kind), "{head:?}");
        }
    }

    #[test]
    fn other_bytes_are_not_detected() {
        for head in [
            &b""[..],
            b"GIF89a",
            b"PK\x03\x04",
            b"{\"unbaked\": 0}",
            // ADTS AAC: sync bits but layer 00.
            b"\xff\xf1\x50\x80",
            b"RIFF\0\0\0\0AVI ",
        ] {
            assert_eq!(detect(head), None, "{head:?}");
        }
    }
}
