//! The facade must expose exactly the paths generated code uses.

use jerrycan::prelude::*;

#[jerrycan::main]
async fn demo_main() -> Result<()> {
    // Never actually served; this test proves the attribute + paths compile.
    let _app = App::new().route("/ping", get(|| async { "pong" }));
    Ok(())
}

#[test]
fn facade_paths_compile() {
    // demo_main is intentionally unused at runtime; its existence is the test.
    let _ = demo_main as fn() -> Result<()>;
}
