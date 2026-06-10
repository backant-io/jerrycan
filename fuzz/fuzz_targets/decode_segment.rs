#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(path) = std::str::from_utf8(data) {
        use jerrycan_core::{get, App};
        let app = App::new()
            .route("/items/{id}", get(|| async { "x" }))
            .route("/a/b/c", get(|| async { "y" }))
            .into_test();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // The router parser only sees the `.path()` of a parsed `http::Uri`
        // (hyper rejects unparseable request-line URIs upstream in production).
        // Guard so libfuzzer exercises jerrycan's decoder, not `http`'s URI
        // validator (whose reject path is a `TestApp` `.expect`, not our bug).
        if jerrycan_core::http::Uri::try_from(path).is_ok() {
            rt.block_on(async {
                let _ = app.get(path).await;
            });
        }
    }
});
