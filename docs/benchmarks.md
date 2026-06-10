# Benchmarks

Informational baselines (criterion, local run on Apple M4 Pro (arm64, macOS), release). Not a CI gate.

| Benchmark | Median |
|---|---|
| dispatch_param_route | 652.41 ns |
| dispatch_404 | 557.15 ns |
| session_encode | 1.1279 µs |
| session_decode | 303.60 ns |
| jwt_encode | 815.26 ns |
| jwt_decode | 962.61 ns |

Run locally: `cargo bench -p jerrycan-core -p jerrycan-auth`.

We deliberately bench only the per-request crypto (session/JWT encode+decode) and the
router dispatch hot path — NOT argon2 password hashing, which is intentionally slow
(a cost parameter, not a hot path) and would dominate any table it appeared in.
