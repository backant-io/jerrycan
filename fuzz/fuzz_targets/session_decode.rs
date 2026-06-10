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
        let store = jerrycan_auth::SessionStore::new(&[7u8; 32]);
        let _ = store.decode::<C>(token);
    }
});
