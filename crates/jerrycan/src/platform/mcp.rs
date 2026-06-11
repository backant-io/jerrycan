//! Hand-rolled MCP server: newline-delimited JSON-RPC 2.0 over stdio.
//! tools/list serves the embedded contract file (all four fields — name,
//! description, inputSchema, outputSchema — are forwarded) — drift is impossible.

use serde_json::{Value, json};
use std::io::{BufRead, Write};

/// The frozen tool contracts, embedded at compile time.
pub const CONTRACTS: &str = include_str!("../../embedded/contracts/mcp-tools.json");

const PROTOCOL_VERSION: &str = "2025-06-18";

/// Hard cap on a single MCP stdio line. A 16 MiB request is already absurd for
/// JSON-RPC; anything larger is treated as hostile/buggy input, not a payload.
pub(crate) const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// The fixed JSON-RPC reply for an oversized line. Hand-written rather than
/// built via serde so it stays a single allocation-free literal.
const OVERSIZED_RESPONSE: &str = r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"request too large (line exceeds 16 MiB)"}}"#;

/// Read one newline-terminated message with a hard size cap, layering the four
/// outcomes so the caller can branch on each:
///
/// * `None` — EOF, no more messages.
/// * `Some(Err(_))` — I/O error from the underlying reader.
/// * `Some(Ok(Err(())))` — the line exceeded `max`; it has already been drained
///   up to (and including) its terminating newline, so the stream stays aligned
///   for the next call.
/// * `Some(Ok(Ok(line)))` — a complete line (newline stripped), decoded lossily
///   so invalid UTF-8 lands in `handle_message` as a -32700 parse error rather
///   than aborting the loop.
pub(crate) fn read_message(
    reader: &mut impl BufRead,
    max: usize,
) -> Option<std::io::Result<Result<String, ()>>> {
    let mut buf: Vec<u8> = Vec::new();
    let mut overflowed = false;
    let mut saw_any = false;

    loop {
        let available = match reader.fill_buf() {
            Ok(b) => b,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Some(Err(e)),
        };
        if available.is_empty() {
            // True EOF: nothing buffered and no bytes seen at all this call.
            if !saw_any {
                return None;
            }
            // EOF with a trailing, unterminated final line. Honor it like
            // `lines()` does: return what we have (or the overflow marker).
            break;
        }
        saw_any = true;

        match available.iter().position(|&b| b == b'\n') {
            Some(nl) => {
                if !overflowed {
                    if buf.len() + nl > max {
                        overflowed = true;
                    } else {
                        buf.extend_from_slice(&available[..nl]);
                    }
                }
                reader.consume(nl + 1); // include the newline
                break;
            }
            None => {
                let len = available.len();
                if !overflowed {
                    if buf.len() + len > max {
                        // Stop storing; keep draining until the newline so the
                        // stream realigns for the next read.
                        overflowed = true;
                    } else {
                        buf.extend_from_slice(available);
                    }
                }
                reader.consume(len);
            }
        }
    }

    if overflowed {
        return Some(Ok(Err(())));
    }
    Some(Ok(Ok(String::from_utf8_lossy(&buf).into_owned())))
}

pub fn serve_stdio() -> Result<(), String> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = stdin.lock();
    while let Some(outcome) = read_message(&mut reader, MAX_LINE_BYTES) {
        let line = match outcome.map_err(|e| e.to_string())? {
            Ok(line) => line,
            Err(()) => {
                // Fail loud but keep serving: emit a JSON-RPC error and read on.
                let mut out = stdout.lock();
                writeln!(out, "{OVERSIZED_RESPONSE}").map_err(|e| e.to_string())?;
                out.flush().map_err(|e| e.to_string())?;
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = handle_message(&line) {
            let mut out = stdout.lock();
            writeln!(out, "{response}").map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn rpc_error(id: Value, code: i64, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

fn rpc_result(id: Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

/// Handle one message; None for notifications (no response).
pub fn handle_message(line: &str) -> Option<String> {
    let Ok(msg) = serde_json::from_str::<Value>(line) else {
        return Some(rpc_error(Value::Null, -32700, "parse error"));
    };
    let id = msg["id"].clone();
    let method = msg["method"].as_str().unwrap_or("");
    let params = &msg["params"];

    if id.is_null() {
        return None; // notification (initialized, cancelled, …): no response
    }
    let result = match method {
        "initialize" => json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "jerrycan", "version": env!("CARGO_PKG_VERSION") },
        }),
        "ping" => json!({}),
        "tools/list" => {
            let contracts: Value =
                serde_json::from_str(CONTRACTS).expect("embedded contract is valid JSON");
            let tools: Vec<Value> = contracts["tools"]
                .as_array()
                .expect("tools array")
                .iter()
                .map(|t| {
                    json!({
                        "name": t["name"],
                        "description": t["description"],
                        "inputSchema": t["inputSchema"],
                        "outputSchema": t["outputSchema"],
                    })
                })
                .collect();
            json!({ "tools": tools })
        }
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or("");
            let (is_error, payload) = super::mcp_dispatch::dispatch(name, &params["arguments"]);
            if is_error {
                json!({
                    "content": [{ "type": "text", "text": payload.to_string() }],
                    "isError": true,
                })
            } else {
                // 2025-06-18 pairs outputSchema with structuredContent; keep the
                // text mirror so non-structured clients still get the payload.
                json!({
                    "content": [{ "type": "text", "text": payload.to_string() }],
                    "structuredContent": payload,
                    "isError": false,
                })
            }
        }
        _ => {
            // A request-shaped notification (has an id) gets a benign ack rather
            // than method-not-found.
            if method.starts_with("notifications/") {
                return Some(rpc_result(id, json!({})));
            }
            return Some(rpc_error(
                id,
                -32601,
                &format!("method not found: {method}"),
            ));
        }
    };
    Some(rpc_result(id, result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn reads_a_normal_small_line() {
        let mut r = Cursor::new(b"hello world\n".to_vec());
        let got = read_message(&mut r, MAX_LINE_BYTES)
            .expect("not EOF")
            .expect("no io error")
            .expect("not oversized");
        assert_eq!(got, "hello world");
    }

    #[test]
    fn oversized_line_is_marked_then_stream_realigns() {
        // A 17 MiB line (over the 16 MiB cap) followed by a normal one. The
        // first read must report oversized; the second must yield the normal
        // line intact, proving the oversized line was drained to its newline.
        let big = vec![b'x'; 17 * 1024 * 1024];
        let mut bytes = big;
        bytes.push(b'\n');
        bytes.extend_from_slice(b"after\n");
        let mut r = Cursor::new(bytes);

        let first = read_message(&mut r, MAX_LINE_BYTES)
            .expect("not EOF")
            .expect("no io error");
        assert_eq!(first, Err(()), "oversized line is marked, not stored");

        let second = read_message(&mut r, MAX_LINE_BYTES)
            .expect("not EOF")
            .expect("no io error")
            .expect("not oversized");
        assert_eq!(second, "after", "stream realigned past the oversized line");
    }

    #[test]
    fn eof_yields_none() {
        let mut r = Cursor::new(Vec::<u8>::new());
        assert!(read_message(&mut r, MAX_LINE_BYTES).is_none());
    }

    #[test]
    fn line_exactly_at_cap_is_accepted() {
        // Boundary: a line of exactly `max` content bytes must NOT be rejected.
        let max = 8;
        let mut r = Cursor::new(b"01234567\nok\n".to_vec());
        let first = read_message(&mut r, max)
            .expect("not EOF")
            .expect("no io error")
            .expect("a line exactly at the cap is allowed");
        assert_eq!(first, "01234567");
        let second = read_message(&mut r, max)
            .expect("not EOF")
            .expect("no io error")
            .expect("not oversized");
        assert_eq!(second, "ok");
    }
}
