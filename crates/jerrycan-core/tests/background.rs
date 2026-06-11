//! Spec: `App::on_serve` background tasks — start under serve, share the
//! serve-time JoinSet (same 10s drain cap), and observe shutdown via a watch
//! channel. TestApp deliberately ignores them: background tasks run only under
//! a real serve, so test logic is driven directly.

use jerrycan_core::{App, get};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

/// Poll a condition every 10ms up to `max` iterations (≈ `max * 10ms`).
/// Avoids brittle fixed sleeps when waiting for a background task to act.
async fn poll_until(max: usize, mut cond: impl FnMut() -> bool) -> bool {
    for _ in 0..max {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    cond()
}

#[tokio::test]
async fn background_tasks_start_on_serve_and_drain_on_shutdown() {
    let started = Arc::new(AtomicBool::new(false));
    let stopped = Arc::new(AtomicBool::new(false));
    let (s2, p2) = (started.clone(), stopped.clone());
    let app = App::new().on_serve("probe", move |_deps, mut shutdown| async move {
        s2.store(true, Ordering::SeqCst);
        let _ = shutdown.changed().await;
        p2.store(true, Ordering::SeqCst);
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let h = tokio::spawn(app.serve_with_shutdown(listener, async {
        let _ = rx.await;
    }));

    assert!(
        poll_until(100, || started.load(Ordering::SeqCst)).await,
        "background task must start when serve begins"
    );
    // The task is parked on shutdown — not yet drained.
    assert!(!stopped.load(Ordering::SeqCst), "must wait for shutdown");

    tx.send(()).unwrap();
    h.await.unwrap().unwrap();
    assert!(
        stopped.load(Ordering::SeqCst),
        "drain must complete before serve returns"
    );
}

#[tokio::test]
async fn background_task_resolves_app_level_deps_via_task_context() {
    // A `provide`d counter must be reachable from a background task's
    // TaskContext — proving app-level deps reach on_serve tasks.
    #[derive(Clone)]
    struct Counter(Arc<AtomicUsize>);
    let seen = Arc::new(AtomicUsize::new(0));
    let observed = seen.clone();

    let app = App::new().provide(Counter(seen.clone())).on_serve(
        "counter",
        move |mut deps, mut shutdown| async move {
            // Resolve an app-level dep, bump it, then park until shutdown.
            let counter = deps.resolve::<Counter>().await.unwrap();
            counter.0.fetch_add(1, Ordering::SeqCst);
            let _ = shutdown.changed().await;
        },
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let h = tokio::spawn(app.serve_with_shutdown(listener, async {
        let _ = rx.await;
    }));

    assert!(
        poll_until(100, || observed.load(Ordering::SeqCst) == 1).await,
        "task must resolve and bump the provided counter"
    );

    tx.send(()).unwrap();
    h.await.unwrap().unwrap();
}

#[tokio::test]
async fn test_app_does_not_run_background_tasks() {
    // `into_test` builds the app WITHOUT serving — background tasks are
    // serve-time-only and must never fire under the in-memory test client.
    let ran = Arc::new(AtomicBool::new(false));
    let flag = ran.clone();
    let t = App::new()
        .route("/ping", get(|| async { "pong" }))
        .on_serve("never", move |_deps, _shutdown| {
            let flag = flag.clone();
            async move {
                flag.store(true, Ordering::SeqCst);
            }
        })
        .into_test();

    // Drive a request; give any (wrongly) spawned task room to run.
    assert_eq!(t.get("/ping").await.text(), "pong");
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        !ran.load(Ordering::SeqCst),
        "TestApp must not run background tasks"
    );
}
