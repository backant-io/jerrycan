//! Shared test harness: drives the real binary over stdio with raw JSON-RPC.
//! Each test binary uses a different subset of these helpers, so unused-in-one
//! is expected — allow dead_code crate-wide for the shared module.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// One shared cargo target dir for every scaffolded conformance/eval app, set as
/// `CARGO_TARGET_DIR` on each app build/check/package/run. The generated apps
/// share a huge, identical dependency tree (tokio/sqlx/sea-orm/hyper/
/// libsqlite3-sys/…); building it into a throwaway `target/` per app compiled it
/// from scratch N times and cost tens of GB. Pointing every app build at ONE dir
/// compiles the deps ONCE and reuses them. Every scaffolded app names its runnable
/// crate `app`, so they all emit the SAME final binary path (`.../debug/app`); the
/// heavy suite therefore runs single-threaded (`--test-threads=1`, set in
/// `heavy.yml`) so only one app builds and serves at a time and that shared output
/// path is never contended. Lives under the repo's `target/` (cleaned by `cargo
/// clean`) and is reused across runs. Override with `JERRYCAN_TEST_TARGET_DIR`.
pub fn shared_app_target() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("JERRYCAN_TEST_TARGET_DIR") {
        return std::path::PathBuf::from(p);
    }
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root")
        .join("target/conformance-apps")
}

/// Give a freshly-scaffolded app a globally-unique runnable-binary name so
/// concurrent builds into the shared `shared_app_target()` can never collide on
/// its final artifact (#118).
///
/// WHY: every scaffolded app is package `app` whose bin is ALSO `app`, so every
/// app uplifts to the SAME `.../debug/app`. Two builds racing into the shared
/// target — a rogue/orphaned `cargo test` beside a gate, or a parallel harness —
/// overwrite each other's `debug/app`, so a served process can exec a STALE
/// binary from a *different* design (phantom `no such column`/404s). Renaming the
/// bin to `app_<uid>` gives each app its own `debug/app_<uid>`; the intermediate
/// `deps/<crate>-<hash>` are already path-hash-distinguished, and the framework
/// deps are the SAME path dep across apps, so they still compile ONCE into the
/// shared target (no per-app framework rebuild). `cargo run -p app` /
/// `cargo build --workspace` / `cargo test --workspace` all select by *package*,
/// so they run/build the sole renamed bin with no serve-site changes.
///
/// `uid` is a hash of the app's absolute path, which carries the tempdir's
/// per-process random component — a stable, cross-process-unique nonce we did not
/// mint from time/`rand`. (The dir *basename* is NOT unique — conformance apps are
/// all `todo-api`, eval apps are the spec name — only the full path is.)
/// Idempotent: a second call for the same app is a no-op.
///
/// Do NOT call this for an app that will run `jerrycan package --binary`: that
/// product path copies `release/app` by its real name, and the package test must
/// keep exercising the faithful `app` bin the real product emits.
pub fn isolate_app_bin(app_dir: &std::path::Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    app_dir
        .canonicalize()
        .unwrap_or_else(|_| app_dir.to_path_buf())
        .hash(&mut h);
    let bin = format!("app_{:016x}", h.finish());
    let manifest = app_dir.join("crates/app/Cargo.toml");
    let toml = std::fs::read_to_string(&manifest).expect("scaffolded app crate Cargo.toml");
    // Idempotent: the scaffold never emits [[bin]]; its presence means we already ran.
    if !toml.contains("[[bin]]") {
        let patched = format!("{toml}\n[[bin]]\nname = \"{bin}\"\npath = \"src/main.rs\"\n");
        std::fs::write(&manifest, patched).expect("rewrite app crate Cargo.toml with unique bin");
    }
    bin
}

pub struct McpClient {
    pub child: Child,
    pub stdin: ChildStdin,
    pub stdout: BufReader<ChildStdout>,
    pub next_id: i64,
}

impl McpClient {
    pub fn start_in(dir: &std::path::Path) -> Self {
        Self::start_in_with_env(dir, &[])
    }

    pub fn start_in_with_env(dir: &std::path::Path, envs: &[(&str, &str)]) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_jerrycan"));
        cmd.arg("mcp")
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
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
            serde_json::json!({"protocolVersion": "2099-01-01", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
        );
        assert_eq!(init["serverInfo"]["name"], "jerrycan");
        // The server answers ITS OWN protocol version, never the client's echo.
        assert_eq!(init["protocolVersion"], "2025-06-18");
        c.notify("notifications/initialized", serde_json::json!({}));
        c
    }

    pub fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
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

    pub fn notify(&mut self, method: &str, params: serde_json::Value) {
        let msg = serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params});
        writeln!(self.stdin, "{msg}").unwrap();
    }

    /// tools/call returning the parsed inner JSON payload.
    pub fn call_tool(&mut self, name: &str, args: serde_json::Value) -> (bool, serde_json::Value) {
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

    pub fn shutdown(mut self) {
        drop(self.stdin);
        let status = self.child.wait().unwrap();
        assert!(status.success(), "clean exit on stdin EOF");
        drop(self.stdout);
    }
}
