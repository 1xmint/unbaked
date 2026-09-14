//! Opening a package ZIP, checking it, and reading every file in it.

#![no_main]

use libfuzzer_sys::fuzz_target;
use unbaked_core::package::{Limits, Package};

fuzz_target!(|zip: &[u8]| {
    let limits = Limits {
        max_entries: 256,
        max_total_size: 16 << 20,
        max_ratio: 100,
    };
    let Ok(mut package) = Package::open(zip, limits) else {
        return;
    };
    let names: Vec<String> = package.file_names().map(str::to_owned).collect();
    for name in &names {
        if let Ok(data) = package.read(name) {
            assert!(data.len() as u64 <= limits.max_total_size);
        }
    }
    let _ = package.verify();
});
