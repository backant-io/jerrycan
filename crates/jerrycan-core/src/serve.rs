//! The hyper serve engine — accept loop, body read, graceful drain.

use crate::app::{App, Policy, apply_security_headers};
use crate::error::{Error, Result};
use crate::response::IntoResponse;
use std::sync::Arc;

/// The serve engine: build the app, accept until `shutdown` resolves, then stop
/// accepting, drain in-flight connections (10s cap), and return.
pub(crate) async fn run_with_shutdown(
    mut app: App,
    listener: tokio::net::TcpListener,
    shutdown: impl std::future::Future<Output = ()> + Send,
) -> Result<()> {
    const DRAIN_CAP: std::time::Duration = std::time::Duration::from_secs(10);
    const HEADER_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

    // Lift background tasks off the builder before `build()` consumes it — they
    // are FnOnce and serve-time-only, so they never enter the shared BuiltApp.
    let background = app.take_background();
    let built = Arc::new(app.build()?);
    let mut connections = tokio::task::JoinSet::new();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    // Launch background tasks into the SAME JoinSet as connections, so they are
    // governed by the identical drain cap. `_name` is retained for future
    // observability (core has no tracing yet). A task that panics yields
    // `Some(Err(JoinError))` from `join_next` — the drain loop still advances,
    // so a panicking task cannot stall graceful shutdown.
    for (_name, factory) in background {
        let fut = factory(built.task_context(), shutdown_rx.clone());
        connections.spawn(fut);
    }

    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => {
                let (stream, _) = match accepted {
                    Ok(pair) => pair,
                    Err(e) if is_transient_accept_error(&e) => {
                        eprintln!("jerrycan: transient accept error ({e}); backing off 50ms");
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        continue;
                    }
                    Err(e) => return Err(Error::internal(format!("accept failed fatally: {e}"))),
                };
                let app = built.clone();
                let write_stall_timeout = built.write_stall_timeout;
                let mut shutdown_rx = shutdown_rx.clone();
                connections.spawn(async move {
                    let io = hyper_util::rt::TokioIo::new(TimedIo::new(stream, write_stall_timeout));
                    let service = hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                        let app = app.clone();
                        async move {
                            let (parts, body) = req.into_parts();
                            // Phase 1: route on the head ALONE. A reject (404/405/400)
                            // answers here — the body is dropped, never read.
                            let limit = match app.route_policy(&parts) {
                                Policy::Reject(response) => {
                                    return Ok::<_, std::convert::Infallible>(response);
                                }
                                Policy::Route { limit } => limit,
                            };
                            // Phase 2: read the body up to THIS route's limit, then dispatch.
                            use http_body_util::BodyExt;
                            let limited = http_body_util::Limited::new(body, limit);
                            let collected =
                                tokio::time::timeout(app.body_read_timeout, limited.collect()).await;
                            let response = match collected {
                                Ok(Ok(collected)) => {
                                    let body = collected.to_bytes();
                                    let app2 = app.clone();
                                    match tokio::spawn(async move { app2.dispatch(parts, body).await }).await {
                                        Ok(response) => response,
                                        Err(_join_error) => {
                                            let mut response =
                                                Error::internal("handler panicked").into_response();
                                            if app.security_headers {
                                                apply_security_headers(&mut response);
                                            }
                                            response
                                        }
                                    }
                                }
                                Ok(Err(_)) => {
                                    let mut response = Error::payload_too_large().into_response();
                                    if app.security_headers {
                                        apply_security_headers(&mut response);
                                    }
                                    response
                                }
                                Err(_) => {
                                    let mut response = Error::new(
                                        http::StatusCode::REQUEST_TIMEOUT,
                                        "JC0408",
                                        "timed out reading the request body",
                                    )
                                    .into_response();
                                    if app.security_headers {
                                        apply_security_headers(&mut response);
                                    }
                                    response
                                }
                            };
                            Ok::<_, std::convert::Infallible>(response)
                        }
                    });
                    let conn = hyper::server::conn::http1::Builder::new()
                        .timer(hyper_util::rt::TokioTimer::new())
                        .header_read_timeout(HEADER_READ_TIMEOUT)
                        .serve_connection(io, service);
                    tokio::pin!(conn);
                    loop {
                        tokio::select! {
                            result = conn.as_mut() => {
                                let _ = result;
                                break;
                            }
                            _ = shutdown_rx.changed() => {
                                // Finish in-flight responses, close idle keep-alives now.
                                conn.as_mut().graceful_shutdown();
                            }
                        }
                    }
                });
            }
        }
    }

    let _ = shutdown_tx.send(true);
    drop(listener); // stop accepting immediately
    let drain = async { while connections.join_next().await.is_some() {} };
    if tokio::time::timeout(DRAIN_CAP, drain).await.is_err() {
        eprintln!("jerrycan: drain cap reached — aborting remaining connections");
        // Aborting a connection task detaches (not aborts) its in-flight dispatch spawn; runaway handlers are still bounded by handler_timeout.
        connections.abort_all();
    }
    Ok(())
}

/// Resolves on Ctrl-C (SIGINT) or, on Unix, SIGTERM — the signals containers
/// and process managers use to request shutdown.
pub(crate) async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler installation never fails on unix");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    eprintln!("jerrycan: shutdown signal received — draining");
}

/// Accept errors that mean "back off and keep serving", not "die":
/// aborted/reset handshakes, signal interruptions, and fd exhaustion
/// (EMFILE/ENFILE — kind-mapping varies by platform, so match raw errno too).
pub(crate) fn is_transient_accept_error(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::Interrupted
            | std::io::ErrorKind::WouldBlock
    ) || matches!(e.raw_os_error(), Some(23) | Some(24))
}

/// Socket wrapper that bounds WRITE stalls. Reads pass through untouched
/// (idle keep-alives legitimately sit in read; hyper's header_read_timeout
/// governs them). The deadline arms when a write/flush returns Pending and
/// resets on progress, so slow-but-moving clients are fine; stalls are not.
pub(crate) struct TimedIo<T> {
    inner: T,
    cap: std::time::Duration,
    stall: Option<std::pin::Pin<Box<tokio::time::Sleep>>>,
}

impl<T> TimedIo<T> {
    pub(crate) fn new(inner: T, cap: std::time::Duration) -> Self {
        Self {
            inner,
            cap,
            stall: None,
        }
    }

    /// Shared Pending arm for `poll_write`/`poll_flush`: the inner write already
    /// registered its waker (it returned Pending), so we also poll the stall
    /// timer to register ITS waker — both wakers live, exactly like
    /// `TimedFrames` in response.rs. The timer firing means the write made no
    /// progress within `cap`: surface a TimedOut error so hyper drops the conn.
    fn poll_stall(
        stall: &mut Option<std::pin::Pin<Box<tokio::time::Sleep>>>,
        cap: std::time::Duration,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        use std::future::Future;
        use std::task::Poll;
        let sleep = stall.get_or_insert_with(|| Box::pin(tokio::time::sleep(cap)));
        match sleep.as_mut().poll(cx) {
            Poll::Ready(()) => {
                *stall = None;
                Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "connection write stalled past the cap",
                )))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for TimedIo<T> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<T: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for TimedIo<T> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        use std::task::Poll;
        match std::pin::Pin::new(&mut self.inner).poll_write(cx, buf) {
            Poll::Ready(r) => {
                self.stall = None;
                Poll::Ready(r)
            }
            Poll::Pending => {
                let cap = self.cap;
                match Self::poll_stall(&mut self.stall, cap, cx) {
                    Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                    Poll::Ready(Ok(())) => unreachable!("poll_stall never returns Ready(Ok)"),
                    Poll::Pending => Poll::Pending,
                }
            }
        }
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        use std::task::Poll;
        match std::pin::Pin::new(&mut self.inner).poll_flush(cx) {
            Poll::Ready(r) => {
                self.stall = None;
                Poll::Ready(r)
            }
            Poll::Pending => {
                let cap = self.cap;
                Self::poll_stall(&mut self.stall, cap, cx)
            }
        }
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
