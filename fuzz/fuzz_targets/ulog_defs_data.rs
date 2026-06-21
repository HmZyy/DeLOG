#![no_main]
//! PX4 ULog definitions + data decoder.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    delog_fuzz::fuzz_ulog(data);
});
