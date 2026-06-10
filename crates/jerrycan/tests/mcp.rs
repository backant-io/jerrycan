//! Drives the real binary over stdio with raw JSON-RPC lines.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl McpClient {
    fn start_in(dir: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
            .arg("mcp")
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut c = Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        let init = c.request(
            "initialize",
            serde_json::json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
        );
        assert_eq!(init["serverInfo"]["name"], "jerrycan");
        c.notify("notifications/initialized", serde_json::json!({}));
        c
    }

    fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let id = self.next_id;
        self.next_id += 1;
        let msg =
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{msg}").unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["id"], id, "response id matches: {v}");
        assert!(v.get("error").is_none(), "unexpected JSON-RPC error: {v}");
        v["result"].clone()
    }

    fn notify(&mut self, method: &str, params: serde_json::Value) {
        let msg = serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params});
        writeln!(self.stdin, "{msg}").unwrap();
    }

    /// tools/call returning the parsed inner JSON payload.
    fn call_tool(&mut self, name: &str, args: serde_json::Value) -> (bool, serde_json::Value) {
        let result = self.request(
            "tools/call",
            serde_json::json!({"name": name, "arguments": args}),
        );
        let is_error = result["isError"].as_bool().unwrap_or(false);
        let text = result["content"][0]["text"].as_str().expect("text content");
        (
            is_error,
            serde_json::from_str(text).expect("payload is JSON"),
        )
    }

    fn shutdown(mut self) {
        drop(self.stdin);
        let status = self.child.wait().unwrap();
        assert!(status.success(), "clean exit on stdin EOF");
        drop(self.stdout);
    }
}

#[test]
fn initialize_list_and_unknown_method() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = McpClient::start_in(tmp.path());

    let tools = c.request("tools/list", serde_json::json!({}));
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 9, "all 9 contract tools served");
    assert!(names.contains(&"jerrycan_design") && names.contains(&"jerrycan_check"));

    // Unknown method → -32601, server keeps running.
    let msg =
        serde_json::json!({"jsonrpc": "2.0", "id": 99, "method": "bogus/method", "params": {}});
    writeln!(c.stdin, "{msg}").unwrap();
    let mut line = String::new();
    c.stdout.read_line(&mut line).unwrap();
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["error"]["code"], -32601);

    let pong = c.request("ping", serde_json::json!({}));
    assert!(pong.as_object().unwrap().is_empty());
    c.shutdown();
}

#[test]
fn docs_tools_work_through_mcp() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = McpClient::start_in(tmp.path());
    let (err, payload) = c.call_tool(
        "jerrycan_docs_search",
        serde_json::json!({"query": "override_dep"}),
    );
    assert!(!err);
    assert_eq!(payload["results"][0]["page"], "testing");
    let (err, payload) = c.call_tool("jerrycan_docs_get", serde_json::json!({"page": "errors"}));
    assert!(!err);
    assert!(payload["markdown"].as_str().unwrap().contains("JC0404"));
    c.shutdown();
}
