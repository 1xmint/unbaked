//! The whole reading path: detect the carrier, open its package, and check it.

#![no_main]

use libfuzzer_sys::fuzz_target;
use unbaked_core::open;
use unbaked_core::package::Limits;

fuzz_target!(|file: &[u8]| {
    let limits = Limits {
        max_entries: 256,
        max_total_size: 16 << 20,
        max_ratio: 100,
    };
    let Ok(opened) = open(file, limits) else {
        return;
    };
    let _ = opened.file_names().count();
    let _ = opened.recipe();
    let _ = opened.check();
});
