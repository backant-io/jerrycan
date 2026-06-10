//! design.json parsing must never panic on garbage (it's agent/file input).

use jerrycan::platform::design::Design;

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
}

const GOLDEN: &str = include_str!("../../../conformance/designs/todo-api.design.json");

#[test]
fn design_parse_never_panics_on_corrupted_golden() {
    let bytes = GOLDEN.as_bytes();
    let mut rng = Rng(0x1234_5678_9ABC_DEF0);
    for _ in 0..20_000 {
        let mut corrupted = bytes.to_vec();
        // Flip / truncate / inject random bytes.
        let ops = (rng.next() % 8) as usize;
        for _ in 0..ops {
            if corrupted.is_empty() {
                break;
            }
            let i = (rng.next() as usize) % corrupted.len();
            corrupted[i] = (rng.next() & 0xff) as u8;
        }
        if rng.next().is_multiple_of(2) && !corrupted.is_empty() {
            corrupted.truncate((rng.next() as usize) % corrupted.len());
        }
        // serde_json::from_slice into Design must Err, never panic.
        let _ = serde_json::from_slice::<Design>(&corrupted);
    }
    let _ = Design::from_path; // anchor
}
