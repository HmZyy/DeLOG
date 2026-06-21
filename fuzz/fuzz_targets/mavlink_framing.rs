#![no_main]
//! MAVLink v1/v2 framing + `.tlog` envelope.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    delog_fuzz::fuzz_mavlink(data);
});
