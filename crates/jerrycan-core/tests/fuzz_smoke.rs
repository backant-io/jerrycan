//! Deterministic randomized smoke: jerrycan-owned parsers must NEVER panic on
//! adversarial input. Fixed seed → reproducible. Deep fuzzing lives in fuzz/.

use jerrycan_core::http::{Method, Uri};

/// xorshift64* — tiny deterministic PRNG (no rand dep in core tests).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    /// A messy path-ish string: %-escapes, slashes, unicode, control bytes.
    fn messy_path(&mut self) -> String {
        let alphabet = b"/abc{}%0123456789ZZ%2%C3%A9%%/../\xff \t";
        let len = (self.next() % 40) as usize;
        let mut s = String::from("/");
        for _ in 0..len {
            let c = alphabet[(self.next() as usize) % alphabet.len()];
            s.push(c as char);
        }
        s
    }
    /// A messy multipart-ish byte string: boundary fragments, CRLFs, header
    /// punctuation, padding, control bytes — the alphabet a real `multipart/
    /// form-data` body draws from, shuffled adversarially.
    fn messy_multipart(&mut self) -> Vec<u8> {
        // Includes the literal boundary token "B" plus the bytes that compose
        // delimiters, headers, padding, and closing markers, so the PRNG can
        // straddle boundaries and partial delimiters.
        let alphabet: &[&[u8]] = &[
            b"\r\n", b"--", b"B", b" ", b"\t", b"content-disposition:", b" form-data;",
            b" name=\"", b"\"", b" filename=\"", b"content-type: text/plain", b"x", b"\0",
            b"\xff", b";", b"=", b"\r", b"\n",
        ];
        let len = (self.next() % 60) as usize;
        let mut b = Vec::new();
        for _ in 0..len {
            b.extend_from_slice(alphabet[(self.next() as usize) % alphabet.len()]);
        }
        b
    }
}

#[test]
fn router_matching_never_panics_on_adversarial_paths() {
    use jerrycan_core::{App, get};
    // A built app with a mix of static + param + nested routes.
    let app = App::new()
        .route("/", get(|| async { "root" }))
        .route("/items/{id}", get(|| async { "item" }))
        .route("/a/b/c", get(|| async { "abc" }))
        .route("/a/{x}/d", get(|| async { "axd" }))
        .into_test();
    // Drive 20k adversarial GETs through real dispatch (router + decode).
    let mut rng = Rng(0x9E3779B97F4A7C15);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        for _ in 0..20_000 {
            let path = {
                let mut r = Rng(rng.next());
                r.messy_path()
            };
            // The router's parsers (decode_segment / Trie::find) only ever see the
            // `.path()` of a successfully-parsed `http::Uri`: in production hyper
            // rejects an unparseable request-line URI before jerrycan is reached,
            // and the in-memory `TestApp.get` builds the same `http::Uri`. So we
            // only drive paths the URI validator accepts — feeding bytes hyper
            // would already reject would test `http`'s validator, not ours. Any
            // status is fine; the contract is "does not panic / hang".
            if Uri::try_from(path.as_str()).is_ok() {
                let _ = app.get(&path).await;
            }
        }
    });
    let _ = Method::GET; // import anchor
}

#[test]
fn multipart_parser_never_panics_on_adversarial_bodies() {
    // The incremental multipart parser is fed adversarial bodies at every
    // chunk straddle and boundary fragment; the contract is "no panic, no
    // hang" — `fuzz_drive`'s linear-time budget assert catches a livelock,
    // any parser panic surfaces here. Mirrors the deep fuzz/ target on a
    // fixed seed so a regression reproduces deterministically.
    let mut rng = Rng(0xD1B54A32D192ED03);
    for _ in 0..20_000 {
        let body = rng.messy_multipart();
        // Sweep a few chunk sizes so the same body exercises different feed
        // straddles; chunk=1 is the worst case for the termination budget.
        for &chunk in &[1usize, 3, 7, body.len().max(1)] {
            jerrycan_core::multipart::fuzz_drive("B", &body, chunk);
        }
    }
}
