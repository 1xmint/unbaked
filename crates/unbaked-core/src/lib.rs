//! Read and write Unbaked media files.

pub mod bake;
pub mod json;
pub mod mp4;
mod open;
pub mod package;
pub mod png;
pub mod recipe;
pub mod rules;
pub mod sniff;

pub use open::{Container, OpenError, Opened, Status, detect, open, sha256_hex};

/// The spec version this crate implements.
pub const SPEC_VERSION: u32 = 0;

/// The kind of carrier file that holds the finished render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Carrier {
    Png,
    M4a,
    Mp4,
}

impl Carrier {
    /// The full file extension, including the `unbaked` marker.
    pub fn extension(self) -> &'static str {
        match self {
            Carrier::Png => "unbaked.png",
            Carrier::M4a => "unbaked.m4a",
            Carrier::Mp4 => "unbaked.mp4",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_keeps_the_player_extension_last() {
        assert_eq!(Carrier::Png.extension(), "unbaked.png");
        assert_eq!(Carrier::M4a.extension(), "unbaked.m4a");
        assert_eq!(Carrier::Mp4.extension(), "unbaked.mp4");
    }
}
