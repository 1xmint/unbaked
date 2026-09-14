//! Reading `bake.json`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use unbaked_core::bake;

fuzz_target!(|json: &[u8]| {
    let _ = bake::parse(json);
});
