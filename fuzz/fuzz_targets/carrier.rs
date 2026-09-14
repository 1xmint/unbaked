//! Finding, removing and writing the hidden package in PNG and MP4 files.

#![no_main]

use libfuzzer_sys::fuzz_target;
use unbaked_core::{detect, mp4, png};

const PACKAGE: &[u8] = b"PK\x05\x06 stands in for a package";

fuzz_target!(|file: &[u8]| {
    let _ = detect(file);

    let _ = png::read_slot(file);
    let png_without = png::without_slot(file);
    if let Ok(written) = png::write_slot(file, PACKAGE) {
        // A written slot reads back, and removing it gives the same render bytes.
        assert_eq!(png::read_slot(&written), Ok(Some(PACKAGE)));
        assert_eq!(png::without_slot(&written), png_without);
    }

    let _ = mp4::read_slot(file);
    let _ = mp4::without_slot(file);
    if let Ok(written) = mp4::write_slot(file, PACKAGE) {
        assert_eq!(mp4::read_slot(&written), Ok(Some(PACKAGE)));
    }
});
