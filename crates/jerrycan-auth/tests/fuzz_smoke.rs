//! Session/JWT decoders must never panic on attacker-controlled bytes.

use jerrycan_auth::{SessionStore, jwt};
use serde::Deserialize;

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
    /// A token-ish string: base64 alphabet, dots, padding, junk.
    fn token(&mut self) -> String {
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.=%\xff";
        let len = (self.next() % 120) as usize;
        let mut s = String::new();
        for _ in 0..len {
            s.push(alphabet[(self.next() as usize) % alphabet.len()] as char);
        }
        s
    }
}

#[derive(Deserialize)]
struct AnyClaims {
    #[allow(dead_code)]
    sub: Option<String>,
}

#[test]
fn session_and_jwt_decode_never_panic() {
    let key = [7u8; 32];
    let store = SessionStore::new(&key);
    let mut rng = Rng(0xDEADBEEFCAFEF00D);
    for _ in 0..50_000 {
        let token = rng.token();
        let _ = store.decode::<AnyClaims>(&token); // must Err, never panic
        let _ = jwt::decode::<AnyClaims>(&token, &key);
    }
    // Also fuzz the cookie-header parser path with junk cookie strings.
    for _ in 0..10_000 {
        let header = rng.token();
        let _ = store.read_cookie(&header);
    }
}
