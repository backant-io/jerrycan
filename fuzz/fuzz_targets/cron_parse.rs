#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // Parse the cron expression and, on success, exercise the bounded
        // forward/backward scans over a spread of instants. The contract is
        // panic-freedom: a malformed expression must Err, never panic.
        jerrycan_jobs::cron::fuzz_drive(s);
    }
});
