//! Hot-path benchmarks: router matching and full in-memory dispatch.
use criterion::{Criterion, criterion_group, criterion_main};
use jerrycan_core::{App, TestApp, get};

fn build_app() -> TestApp {
    let mut app = App::new();
    for i in 0..50 {
        let path = format!("/resource{i}/{{id}}");
        app = app.route(&path, get(|| async { "x" })); // route() stores an owned String; &str is fine
    }
    app.into_test()
}

fn bench_dispatch(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let app = build_app();
    c.bench_function("dispatch_param_route", |b| {
        b.iter(|| rt.block_on(async { app.get("/resource25/42").await }));
    });
    c.bench_function("dispatch_404", |b| {
        b.iter(|| rt.block_on(async { app.get("/nope/nope").await }));
    });
}

criterion_group!(benches, bench_dispatch);
criterion_main!(benches);
