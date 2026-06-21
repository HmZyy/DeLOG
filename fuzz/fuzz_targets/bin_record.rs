#![no_main]
//! ArduPilot `.BIN` record decoder.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    delog_fuzz::fuzz_ardupilot(data);
});
