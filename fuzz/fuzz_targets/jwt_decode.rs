#![no_main]
use libfuzzer_sys::fuzz_target;
use serde::Deserialize;

#[derive(Deserialize)]
struct C {
    #[allow(dead_code)]
    sub: Option<String>,
}

fuzz_target!(|data: &[u8]| {
    if let Ok(token) = std::str::from_utf8(data) {
        let _ = jerrycan_auth::jwt::decode::<C>(token, &[7u8; 32]);
    }
});
