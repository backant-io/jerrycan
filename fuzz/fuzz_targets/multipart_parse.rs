#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    // First byte picks the chunking (1..=64), second picks the boundary —
    // sometimes one the input can actually contain, sometimes not.
    let chunk = (data[0] % 64) as usize + 1;
    let boundary = match data[1] % 3 {
        0 => "B",
        1 => "XbOuNdArYx",
        _ => "0123456789012345678901234567890123456789012345678901234567890123456789",
    };
    jerrycan_core::multipart::fuzz_drive(boundary, &data[2..], chunk);
});
