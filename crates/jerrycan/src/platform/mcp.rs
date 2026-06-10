//! Hand-rolled MCP server: newline-delimited JSON-RPC 2.0 over stdio.
//! tools/list serves the embedded contract file (all four fields — name,
//! description, inputSchema, outputSchema — are forwarded) — drift is impossible.

use serde_json::{Value, json};
use std::io::{BufRead, Write};

/// The frozen tool contracts, embedded at compile time.
pub const CONTRACTS: &str = include_str!("../../../../docs/contracts/mcp-tools.json");

const PROTOCOL_VERSION: &str = "2025-06-18";

pub fn serve_stdio() -> Result<(), String> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
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
