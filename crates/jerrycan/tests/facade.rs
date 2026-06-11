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

// A custom id newtype joins the Path param set through the facade-exported
// macro. This pins that `jerrycan::path_param!` resolves through the facade
// (the macro lands at the jerrycan_core root via `#[macro_export]`; the facade
// re-exports it) and that a single `Path<T>` still binds the leaf-most param.
#[derive(Debug)]
struct LeadId(i64);
impl std::str::FromStr for LeadId {
    type Err = std::num::ParseIntError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(LeadId(s.parse()?))
    }
}
jerrycan::path_param!(LeadId);

#[tokio::test]
async fn path_param_macro_resolves_through_the_facade() {
    async fn show(Path(id): Path<LeadId>) -> Result<Json<i64>> {
        Ok(Json(id.0))
    }
    let t = App::new()
        .mount(
            "/ws/{ws}",
            Module::new("leads").route("/leads/{id}", get(show)),
        )
        .into_test();
    assert_eq!(t.get("/ws/7/leads/42").await.json::<i64>(), 42);
}
