//! The hyper serve engine — accept loop, body read, graceful drain.

use crate::app::{App, apply_security_headers};
use crate::error::{Error, Result};
use crate::response::IntoResponse;
use std::sync::Arc;

/// The serve engine: build the app, accept until `shutdown` resolves, then stop
/// accepting, drain in-flight connections (10s cap), and return.
pub(crate) async fn run_with_shutdown(
    app: App,
    listener: tokio::net::TcpListener,
    shutdown: impl std::future::Future<Output = ()> + Send,
) -> Result<()> {
    const BODY_LIMIT: usize = 1024 * 1024; // 1 MiB — spec §4.4
    const DRAIN_CAP: std::time::Duration = std::time::Duration::from_secs(10);
    const HEADER_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

    let built = Arc::new(app.build()?);
    let mut connections = tokio::task::JoinSet::new();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
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
                let mut shutdown_rx = shutdown_rx.clone();
                connections.spawn(async move {
                    let io = hyper_util::rt::TokioIo::new(stream);
                    let service = hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                        let app = app.clone();
                        async move {
                            let (parts, body) = req.into_parts();
                            use http_body_util::BodyExt;
                            let limited = http_body_util::Limited::new(body, BODY_LIMIT);
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
