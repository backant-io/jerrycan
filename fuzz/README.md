# Deep fuzzing (nightly)

```
rustup toolchain install nightly
cargo install cargo-fuzz
cargo +nightly fuzz run decode_segment -- -max_total_time=120
cargo +nightly fuzz run session_decode -- -max_total_time=120
cargo +nightly fuzz run jwt_decode    -- -max_total_time=120
cargo +nightly fuzz run design_parse  -- -max_total_time=120
```

The stable `tests/fuzz_smoke.rs` suites run the same surfaces continuously in CI; this crate is for deeper soak runs. Any crash found here = a parser bug; reproduce, fix the parser, commit the crashing input to the corpus.

This crate is **outside** the stable `[workspace]` (`exclude = ["fuzz"]`), so the
normal `cargo {check,clippy,test} --workspace` gates never build it; it needs
nightly + libfuzzer.
