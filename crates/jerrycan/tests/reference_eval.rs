//! The Reference live HTTP battery — the permanent v2.5 eval gate.
//!
//! This replays the Phase 5a reference backend (`conformance/eval/fixtures/reference`)
//! exactly per its README: scaffold the reference design → `gen-tests` each module →
//! copy the 10 reference files to their destinations → patch the app's Cargo
//! features to add `oauth,mock-idp` → `jerrycan check` → run the generated
//! acceptance suite → serve the app live on a free port (sqlite file DB) → drive
//! every v2 feature over raw HTTP. It also drives both declared crons under a
//! controlled clock and answers data-structure questions from `schema.json`
//! alone. Nothing here mocks the framework: the app is the real generated binary
//! and every assertion is observed over a real `TcpStream` (or the real
//! `schema.json`/`jerrycan_jobs::cron` primitives).
//!
//! Mirrors `tests/eval.rs` (scaffold + live-serve + raw-HTTP) and the live-server
//! precedents in `tests/conformance.rs`. One `#[ignore]`d test because it
//! scaffolds, builds, and serves a full SeaORM backend.
//!
//! Compile note: this test uses `jerrycan::auth::webhook::sign_sha256_hex` and
//! `jerrycan::jobs::{CronSchedule, due_fire}` and
//! `jerrycan::platform::schema::SchemaContract`, so it must be built with the
//! `auth`, `jobs`, and (default) `cli` features. CI runs it `--all-features`.
//!
//! The whole file is gated on those features so a default-feature
//! `cargo test --workspace` compiles it to an empty (0-test) binary instead of
//! failing on the missing `jerrycan::auth`/`jerrycan::jobs` paths.
#![cfg(all(feature = "auth", feature = "jobs"))]

use jerrycan::auth::webhook::sign_sha256_hex;
use jerrycan::jobs::{CronSchedule, due_fire};
use jerrycan::platform::schema::SchemaContract;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, UNIX_EPOCH};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}
fn jc() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_jerrycan"))
}
/// The framework path-dep the scaffolder embeds (the child reads it from env).
fn framework_dep() -> String {
    format!(
        "jerrycan = {{ path = \"{}\", default-features = false }}",
        repo_root().join("crates/jerrycan").display()
    )
}

/// The fixed test secret the live app signs sessions with, matched in-test where
/// we need to read a cookie. Long enough for the auth secret floor.
const SECRET: &str = "reference-battery-secret-string-very-long-1234";

/// The README battery hooks: the webhook signing secret default and the fixed
/// mock OAuth code `connect` re-issues. Kept here so the assertions read against
/// the documented contract, not magic strings.
const WEBHOOK_SECRET: &str = "whsec_reference_reference_secret";
const MOCK_CODE: &str = "reference-mock-code";

#[test]
#[ignore = "heavy: scaffolds/builds/serves the reference backend"]
fn reference_slice_live_battery() {
    let tmp = tempfile::tempdir().unwrap();
    let app = tmp.path().join("reference");

    // ---- 1. Scaffold + gen-tests + apply fixtures + feature patch -----------
    scaffold_and_apply_fixtures(&app);

    // ---- 2. `jerrycan --json check` → ok:true (the full gate) ---------------
    let check = Command::new(jc())
        .current_dir(&app)
        .env("JERRYCAN_FRAMEWORK_DEP", framework_dep())
        .args(["--json", "check"])
        .output()
        .expect("run jerrycan check");
    let payload: serde_json::Value =
        serde_json::from_slice(&check.stdout).expect("check emits json");
    assert_eq!(
        payload["ok"], true,
        "jerrycan check must be green:\n{}",
        payload["diagnostics"]
    );

    // ---- 3. Generated acceptance suite green (cross-tenant isolation) -------
    // The leads module's generated `tenant_a_cannot_read_tenant_b_leads` /
    // `…_api_keys` are the *generated* cross-tenant guarantee; the jobs
    // `acceptance.rs` runs BOTH cron task fns (expire_trials + overdue_callbacks)
    // against the jobs-only DB. `--no-fail-fast` so every binary runs.
    let t_cold = Instant::now();
    let suite = Command::new("cargo")
        .current_dir(&app)
        .args(["test", "--workspace", "--no-fail-fast"])
        .output()
        .expect("run generated acceptance suite");
    let cold_build = t_cold.elapsed();
    let suite_out = format!(
        "{}{}",
        String::from_utf8_lossy(&suite.stdout),
        String::from_utf8_lossy(&suite.stderr)
    );
    assert!(
        suite.status.success(),
        "generated acceptance suite (incl. cross-tenant isolation) must be green:\n{suite_out}"
    );
    // The leads isolation tests are the spec's named guarantee — fail loud if the
    // generator stopped emitting them.
    assert!(
        suite_out.contains("tenant_a_cannot_read_tenant_b_leads"),
        "the generated cross-tenant isolation test must run:\n{suite_out}"
    );

    // ---- 7 (recorded early). Cold-build time to stderr ----------------------
    // The acceptance run above is the first full compile of the generated
    // workspace in this tempdir's own target root — a genuine cold build.
    eprintln!("reference-slice cold build (acceptance suite, from scratch): {cold_build:?}");

    // ---- 4. Serve live + raw-HTTP battery -----------------------------------
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let addr = format!("127.0.0.1:{port}");
    let db_file = app.join("live.db");
    let _ = std::fs::remove_file(&db_file);
    // `sqlite://<path>?mode=rwc` so sqlx CREATES the file (a bare `sqlite://path`
    // tries to OPEN an existing file → "unable to open database file"). The
    // generated app auto-runs MIGRATIONS + JOBS_MIGRATIONS on startup, so a fresh
    // file is fully provisioned by the time it binds the port.
    let db_url = format!("sqlite://{}?mode=rwc", db_file.display());
    let mut server = Command::new("cargo")
        .current_dir(&app)
        .env("JERRYCAN_ADDR", &addr)
        .env("JERRYCAN_SECRET", SECRET)
        .env("JERRYCAN_DATABASE_URL", &db_url)
        .args(["run", "-p", "app"])
        .spawn()
        .expect("spawn live server");

    // Run the battery; ALWAYS kill the server afterwards, even on a panic, so the
    // tempdir can be cleaned up and the port released.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        await_listen(&addr, 180);
        run_http_battery(&addr);
    }));
    let _ = server.kill();
    let _ = server.wait();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }

    // ---- 5. Crons under the test clock --------------------------------------
    crons_fire_under_test_clock();

    // ---- 6. schema.json answers the structural questions --------------------
    schema_answers_structural_questions(&app);

    eprintln!("reference live battery: PASS (all v2 features over real HTTP)");
}

// ============================ replay (step 1) ===============================

/// Replay the fixtures README precisely: scaffold → gen-tests per module → copy
/// the 10 reference files to their destinations → patch the app Cargo features.
fn scaffold_and_apply_fixtures(app: &Path) {
    // scaffold (wired to the local framework path dep; sqlite by default)
    let st = Command::new(jc())
        .env("JERRYCAN_FRAMEWORK_DEP", framework_dep())
        .arg("new")
        .arg(app)
        .arg("--design")
        .arg(repo_root().join("conformance/designs/reference-slice.design.json"))
        .status()
        .expect("scaffold reference");
    assert!(st.success(), "reference design must scaffold");

    // gen-tests per module (makes the generated acceptance suite, incl. the
    // cross-tenant isolation tests, present and runnable).
    for m in [
        "users",
        "workspaces",
        "leads",
        "api-keys",
        "billing",
        "integrations",
    ] {
        let st = Command::new(jc())
            .current_dir(app)
            .args(["gen-tests", "--module", m])
            .status()
            .unwrap_or_else(|e| panic!("gen-tests {m}: {e}"));
        assert!(st.success(), "gen-tests {m} must succeed");
    }

    // Copy each fixture to its README destination.
    let fx = repo_root().join("conformance/eval/fixtures/reference");
    let copies: &[(&str, &str)] = &[
        ("users_handlers.rs", "crates/routes/users/src/handlers.rs"),
        (
            "workspaces_handlers.rs",
            "crates/routes/workspaces/src/handlers.rs",
        ),
        ("leads_handlers.rs", "crates/routes/leads/src/handlers.rs"),
        (
            "api-keys_handlers.rs",
            "crates/routes/api-keys/src/handlers.rs",
        ),
        (
            "billing_handlers.rs",
            "crates/routes/billing/src/handlers.rs",
        ),
        (
            "integrations_handlers.rs",
            "crates/routes/integrations/src/handlers.rs",
        ),
        ("api-keys_deps.rs", "crates/routes/api-keys/src/deps.rs"),
        (
            "integrations_deps.rs",
            "crates/routes/integrations/src/deps.rs",
        ),
        ("jobs_expire_trials.rs", "crates/jobs/src/expire_trials.rs"),
        (
            "jobs_overdue_callbacks.rs",
            "crates/jobs/src/overdue_callbacks.rs",
        ),
    ];
    for (fixture, dest) in copies {
        let to = app.join(dest);
        assert!(
            to.parent().is_some_and(Path::exists),
            "scaffold must have created the destination dir for {dest}"
        );
        std::fs::copy(fx.join(fixture), &to)
            .unwrap_or_else(|e| panic!("copy fixture {fixture} → {dest}: {e}"));
    }

    // Cargo feature patch: `oauth` and `realtime` are wired AUTOMATICALLY (the
    // reference design declares the `oauth` dependency and a `realtime` block), so
    // only `mock-idp` — the test-only IdP harness, never a production dependency —
    // needs adding so the integrations module's hermetic mock transport compiles
    // in this test.
    let cargo = app.join("Cargo.toml");
    let before = r#"features = ["db", "validate", "auth", "observe", "jobs", "oauth", "realtime"]"#;
    let after = r#"features = ["db", "validate", "auth", "observe", "jobs", "oauth", "realtime", "mock-idp"]"#;
    let txt = std::fs::read_to_string(&cargo).expect("read app Cargo.toml");
    assert!(
        txt.contains(before),
        "the scaffolded Cargo.toml must carry the auto-wired oauth feature set to patch"
    );
    std::fs::write(&cargo, txt.replace(before, after)).expect("patch app Cargo.toml features");
}

// ============================ HTTP battery (step 4) =========================

/// Drive every v2 feature over raw HTTP against the live server, asserting each.
fn run_http_battery(addr: &str) {
    // -- a. register two users; login each → capture session cookies ----------
    // The generated `User` DTO requires every field (no serde defaults), and the
    // generated repo `insert`s the body's `id` literally — so each user gets a
    // DISTINCT id (else the second collides on the PK, not the email). This is a
    // request-shape detail of the replay, not a behaviour assertion.
    assert_status(
        post_json(
            addr,
            "/users/register",
            "",
            r#"{"id":1,"email":"a@reference.test","password":"pwAAAA123","role":"user"}"#,
        ),
        201,
        "register user A",
    );
    assert_status(
        post_json(
            addr,
            "/users/register",
            "",
            r#"{"id":2,"email":"b@reference.test","password":"pwBBBB123","role":"user"}"#,
        ),
        201,
        "register user B",
    );
    // A duplicate email is a 409 (the unique index → JC0409) — proves register
    // is really hitting the DB, not echoing.
    assert_status(
        post_json(
            addr,
            "/users/register",
            "",
            r#"{"id":3,"email":"a@reference.test","password":"pwAAAA123","role":"user"}"#,
        ),
        409,
        "duplicate email → 409",
    );

    let cookie_a = login(addr, "a@reference.test", "pwAAAA123");
    let cookie_b = login(addr, "b@reference.test", "pwBBBB123");
    assert!(
        cookie_a.starts_with("jerrycan_session=") && cookie_b.starts_with("jerrycan_session="),
        "login must mint jerrycan_session cookies (A={cookie_a}, B={cookie_b})"
    );

    // -- b. tenant isolation: A creates ws A, B creates ws B, A creates a lead -
    assert_status(
        post_json(
            addr,
            "/workspaces/",
            &cookie_a,
            r#"{"id":1,"name":"Acme","plan":"trial"}"#,
        ),
        201,
        "A creates workspace A (owner member seeded)",
    );
    assert_status(
        post_json(
            addr,
            "/workspaces/",
            &cookie_b,
            r#"{"id":2,"name":"Beta","plan":"trial"}"#,
        ),
        201,
        "B creates workspace B",
    );
    // A creates a lead in A's tenant. The body carries workspace_id to satisfy
    // the DTO; the handler OVERRIDES it with the authenticated tenant's id.
    assert_status(
        post_json(
            addr,
            "/leads/",
            &cookie_a,
            r#"{"id":1,"workspace_id":1,"phone":"+15550001111","name":"Lead One","status":"new","custom":null}"#,
        ),
        201,
        "A creates a lead in workspace A",
    );
    // Live cross-tenant isolation: A reads its lead (200); B cannot (404) and the
    // lead is absent from B's list.
    assert_status(
        get(addr, "/leads/1", &cookie_a),
        200,
        "A reads its own lead",
    );
    assert_status(
        get(addr, "/leads/1", &cookie_b),
        404,
        "B cannot read A's lead (cross-tenant 404)",
    );
    let b_list = get(addr, "/leads/", &cookie_b);
    assert_status(b_list.clone(), 200, "B lists its (empty) leads");
    assert!(
        b_list.body.trim() == "[]",
        "A's lead must be absent from B's list, got: {}",
        b_list.body
    );

    // -- c. billing webhook: none/wrong/correct signature --------------------
    let raw = r#"{"id":"evt_1","type":"invoice.paid"}"#;
    assert_status(
        post_json(addr, "/billing/webhook", "", raw),
        200,
        "webhook with NO signature → 200 (unsigned ping)",
    );
    assert_status(
        post_raw(
            addr,
            "/billing/webhook",
            raw,
            &[("Stripe-Signature", "deadbeefnotvalid")],
        ),
        400,
        "webhook with a WRONG signature → 400",
    );
    // Correct HMAC-SHA256 hex over the RAW body, computed via the framework's own
    // signer (the same primitive the handler verifies with).
    let sig = sign_sha256_hex(WEBHOOK_SECRET.as_bytes(), raw.as_bytes());
    assert_status(
        post_raw(addr, "/billing/webhook", raw, &[("Stripe-Signature", &sig)]),
        200,
        "webhook with a CORRECT signature → 200",
    );

    // -- d. multipart CSV import (2 rows) → 202; rows appear in A's list ------
    let csv = "phone,name,status\n+15551112222,CsvOne,new\n+15553334444,CsvTwo,called\n";
    assert_status(
        post_multipart(addr, "/leads/import", &cookie_a, "file", "leads.csv", csv),
        202,
        "multipart CSV import → 202",
    );
    let a_list = get(addr, "/leads/", &cookie_a);
    assert_status(a_list.clone(), 200, "A lists leads after import");
    assert!(
        a_list.body.contains("+15551112222") && a_list.body.contains("+15553334444"),
        "the 2 imported rows must appear in A's leads, got: {}",
        a_list.body
    );

    // -- e. API-key scopes: with-scope 200, wrong-scope 403, unknown 401 -----
    let key_with = create_api_key(addr, &cookie_a, 11, "battery-read", "leads:read");
    assert_status(
        get_bearer(addr, "/api-keys/usage", &key_with),
        200,
        "scoped key WITH leads:read → 200",
    );
    let key_without = create_api_key(addr, &cookie_a, 12, "battery-noscope", "billing:read");
    assert_status(
        get_bearer(addr, "/api-keys/usage", &key_without),
        403,
        "scoped key LACKING leads:read → 403",
    );
    assert_status(
        get_bearer(
            addr,
            "/api-keys/usage",
            "sk_live_totally-bogus-not-a-real-key",
        ),
        401,
        "an unknown key → 401",
    );

    // -- f. OAuth: connect → 302 with state; callback exchanges via mock ------
    let connect = get(addr, "/integrations/auth/google/connect", "");
    assert_status(connect.clone(), 302, "OAuth connect → 302");
    let location = connect
        .header("location")
        .expect("connect must set a Location header");
    let state = location
        .split("state=")
        .nth(1)
        .and_then(|s| s.split('&').next())
        .expect("Location must carry a state param")
        .to_string();
    assert!(!state.is_empty(), "the OAuth state must be non-empty");
    // Drive the callback with the fixed mock code `connect` re-issued + the state.
    assert_status(
        get(
            addr,
            &format!("/integrations/auth/google/callback?code={MOCK_CODE}&state={state}"),
            "",
        ),
        200,
        "OAuth callback with the mock code → 200 (token exchanged + stored)",
    );
    assert_status(
        get(
            addr,
            &format!("/integrations/auth/google/callback?code=not-a-real-code&state={state}"),
            "",
        ),
        400,
        "OAuth callback with a bad code → 400",
    );
}

/// POST /users/login and return the `jerrycan_session=…` cookie pair.
fn login(addr: &str, email: &str, password: &str) -> String {
    let body = format!(r#"{{"email":"{email}","password":"{password}"}}"#);
    let res = post_json(addr, "/users/login", "", &body);
    assert_status(res.clone(), 200, "login must succeed for valid credentials");
    res.set_cookie()
        .unwrap_or_else(|| panic!("login for {email} must set a session cookie:\n{}", res.raw))
}

/// POST /api-keys/ and return the one-time plaintext key.
fn create_api_key(addr: &str, cookie: &str, id: i64, label: &str, scopes: &str) -> String {
    let body = format!(
        r#"{{"id":{id},"workspace_id":1,"prefix":"ignored","label":"{label}","scopes":"{scopes}"}}"#
    );
    let res = post_json(addr, "/api-keys/", cookie, &body);
    assert_status(res.clone(), 201, "create_api_key must mint a key");
    let v: serde_json::Value =
        serde_json::from_str(&res.body).expect("create_api_key returns JSON");
    v["plaintext"]
        .as_str()
        .expect("the one-time plaintext key must be returned")
        .to_string()
}

// ============================ crons (step 5) ================================

/// Drive BOTH reference crons under a hand-advanced clock and assert each becomes
/// due after its interval.
///
/// WHY this approach (chosen for faithfulness + determinism): a *live* server's
/// cron leader polls real wall time, so an hourly / 5-minute schedule cannot be
/// observed firing within a short test. The framework's own scheduling decision
/// is the pure function `jerrycan_jobs::cron::due_fire(schedule, last_fired,
/// now)` — the EXACT primitive the live cron leader calls each tick (see
/// `jerrycan-jobs/src/lib.rs`, where the worker maps `due_fire` over the cron
/// rows). Driving it with the two real reference schedules and a controlled `now`
/// exercises the same decision logic the server uses, with zero wall-clock wait.
/// The *task bodies* themselves are separately proven green by the generated
/// `crates/jobs/tests/acceptance.rs` run in step 3 (both `expire_trials` and
/// `overdue_callbacks` execute against the jobs-only DB).
fn crons_fire_under_test_clock() {
    // The two declared reference schedules, verbatim from the design.
    let expire_trials = CronSchedule::parse("0 * * * *").expect("expire_trials cron parses");
    let overdue_callbacks =
        CronSchedule::parse("*/5 * * * *").expect("overdue_callbacks cron parses");

    // A minute-aligned base instant that is itself a tick of BOTH schedules
    // (2026-06-15T00:00:00Z: epoch secs ≡ 0 mod 3600 and mod 300), so the math is
    // unambiguous. last_fired = base; advancing `now` makes each schedule due
    // exactly when `now` reaches its next tick — and not before.
    let base = UNIX_EPOCH + Duration::from_secs(1_781_481_600);
    let base_secs = base.duration_since(UNIX_EPOCH).unwrap().as_secs();
    assert_eq!(base_secs % 3600, 0, "base must align to the hourly tick");
    assert_eq!(base_secs % 300, 0, "base must align to the 5-minute tick");

    // expire_trials (hourly): NOT due 30 min later, DUE 1 hour later.
    assert!(
        due_fire(
            &expire_trials,
            Some(base),
            base + Duration::from_secs(30 * 60)
        )
        .is_none(),
        "expire_trials must not be due 30 min after its last fire"
    );
    assert!(
        due_fire(
            &expire_trials,
            Some(base),
            base + Duration::from_secs(60 * 60)
        )
        .is_some(),
        "expire_trials must become due one hour after its last fire"
    );

    // overdue_callbacks (every 5 min): NOT due 4 min later, DUE 5 min later.
    assert!(
        due_fire(
            &overdue_callbacks,
            Some(base),
            base + Duration::from_secs(4 * 60)
        )
        .is_none(),
        "overdue_callbacks must not be due 4 min after its last fire"
    );
    assert!(
        due_fire(
            &overdue_callbacks,
            Some(base),
            base + Duration::from_secs(5 * 60)
        )
        .is_some(),
        "overdue_callbacks must become due five minutes after its last fire"
    );

    // First-run policy: a never-fired cron (NULL last_fired) fires its most-recent
    // tick immediately on the leader's first pass — both reference crons included.
    let later = base + Duration::from_secs(2 * 60);
    assert!(
        due_fire(&expire_trials, None, later).is_some(),
        "expire_trials fires its most-recent tick on first run"
    );
    assert!(
        due_fire(&overdue_callbacks, None, later).is_some(),
        "overdue_callbacks fires its most-recent tick on first run"
    );
}

// ============================ schema.json Q&A (step 6) ======================

/// Load the scaffold's `schema.json`, parse it into the framework's published
/// `SchemaContract`, and assert it answers the structural questions the eval
/// poses — proving schema.json ALONE is sufficient to answer data-structure
/// questions without reading the code.
fn schema_answers_structural_questions(app: &Path) {
    let raw = std::fs::read_to_string(app.join("schema.json")).expect("read schema.json");
    let contract: SchemaContract = serde_json::from_str(&raw).expect("schema.json parses");

    let table = |name: &str| {
        contract
            .tables
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("schema.json must describe table `{name}`"))
    };

    // Q: leads has a FK workspace_id → workspaces.id ON DELETE cascade.
    let leads = table("leads");
    let ws_fk = leads
        .foreign_keys
        .iter()
        .find(|f| f.column == "workspace_id")
        .expect("leads.workspace_id must be a declared FK");
    assert_eq!(
        ws_fk.references.table, "workspaces",
        "leads FK target table"
    );
    assert_eq!(ws_fk.references.column, "id", "leads FK target column");
    assert_eq!(ws_fk.on_delete, "cascade", "leads FK on_delete policy");

    // Q: leads.phone is unique AND indexed.
    assert!(
        leads.unique.iter().any(|u| u == &["phone".to_string()]),
        "leads.phone must be unique"
    );
    assert!(
        leads.indexes.iter().any(|i| i.contains("phone")),
        "leads.phone must be indexed"
    );

    // Q: leads.status is an enum {new, called, dnc}.
    let status = leads
        .enums
        .get("status")
        .expect("leads.status must declare its enum values");
    assert_eq!(
        status,
        &vec!["new".to_string(), "called".to_string(), "dnc".to_string()],
        "leads.status enum values"
    );

    // Q: api_keys FK → workspaces (cascade), and prefix is unique.
    let apikeys = table("apikeys");
    let ak_fk = apikeys
        .foreign_keys
        .iter()
        .find(|f| f.column == "workspace_id")
        .expect("apikeys.workspace_id must be a declared FK");
    assert_eq!(ak_fk.references.table, "workspaces", "apikeys FK target");
    assert_eq!(ak_fk.on_delete, "cascade", "apikeys FK on_delete");
    assert!(
        apikeys.unique.iter().any(|u| u == &["prefix".to_string()]),
        "apikeys.prefix must be unique"
    );

    // Q: users.email is unique; the workspace_members FK to workspaces is a REAL,
    // DB-enforced constraint (same module as the tenant), unlike the cross-module
    // leads/apikeys relations which are application-enforced.
    assert!(
        table("users")
            .unique
            .iter()
            .any(|u| u == &["email".to_string()]),
        "users.email must be unique"
    );
    let members_fk = table("workspace_members")
        .foreign_keys
        .iter()
        .find(|f| f.column == "workspace_id")
        .expect("workspace_members must FK to workspaces");
    assert!(
        members_fk.enforced,
        "the membership FK to workspaces must be a real DB constraint (enforced:true)"
    );
    assert!(
        !ws_fk.enforced,
        "the cross-module leads→workspaces relation is application-enforced (enforced:false)"
    );
}

// ============================ raw HTTP plumbing =============================

/// A parsed HTTP response: status line, raw text, and a lowercased header map for
/// the headers we assert on (Set-Cookie, Location).
#[derive(Clone)]
struct HttpResponse {
    status: u16,
    raw: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl HttpResponse {
    fn parse(raw: String) -> Self {
        let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((raw.as_str(), ""));
        let mut lines = head.lines();
        let status = lines
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let headers = lines
            .filter_map(|l| {
                l.split_once(':')
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            })
            .collect();
        Self {
            status,
            raw: raw.clone(),
            headers,
            body: body.to_string(),
        }
    }
    /// The first value of a (lowercased) header.
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
    /// The `jerrycan_session=…` cookie pair from Set-Cookie (attributes stripped).
    fn set_cookie(&self) -> Option<String> {
        let raw = self.header("set-cookie")?;
        let pair = raw.split(';').next()?.trim();
        pair.starts_with("jerrycan_session=")
            .then(|| pair.to_string())
    }
}

/// Assert a response's status, with a labelled message carrying the raw response.
fn assert_status(res: HttpResponse, want: u16, what: &str) {
    assert_eq!(
        res.status, want,
        "{what}: expected {want}, got {}:\n{}",
        res.status, res.raw
    );
}

fn await_listen(addr: &str, secs: u64) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if TcpStream::connect(addr).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    panic!("reference app did not start listening on {addr} within {secs}s");
}

/// Send a fully-formed request and parse the response.
fn send(addr: &str, request: &[u8]) -> HttpResponse {
    let mut s = TcpStream::connect(addr).expect("connect to live app");
    s.write_all(request).expect("write request");
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    HttpResponse::parse(String::from_utf8_lossy(&buf).into_owned())
}

/// GET with an optional `Cookie:` header (pass "" for none).
fn get(addr: &str, path: &str, cookie: &str) -> HttpResponse {
    let cookie_line = if cookie.is_empty() {
        String::new()
    } else {
        format!("Cookie: {cookie}\r\n")
    };
    let req = format!("GET {path} HTTP/1.1\r\nHost: l\r\n{cookie_line}Connection: close\r\n\r\n");
    send(addr, req.as_bytes())
}

/// GET with an `Authorization: Bearer <key>` header (the API-key path).
fn get_bearer(addr: &str, path: &str, key: &str) -> HttpResponse {
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: l\r\nAuthorization: Bearer {key}\r\nConnection: close\r\n\r\n"
    );
    send(addr, req.as_bytes())
}

/// POST a JSON body with an optional `Cookie:` header (pass "" for none).
fn post_json(addr: &str, path: &str, cookie: &str, body: &str) -> HttpResponse {
    let cookie_line = if cookie.is_empty() {
        String::new()
    } else {
        format!("Cookie: {cookie}\r\n")
    };
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: l\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{cookie_line}Connection: close\r\n\r\n{body}",
        body.len()
    );
    send(addr, req.as_bytes())
}

/// POST a raw body with arbitrary extra headers (used for the signed webhook).
fn post_raw(addr: &str, path: &str, body: &str, extra: &[(&str, &str)]) -> HttpResponse {
    let extra_lines: String = extra.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: l\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra_lines}Connection: close\r\n\r\n{body}",
        body.len()
    );
    send(addr, req.as_bytes())
}

/// POST a `multipart/form-data` body with a single file part.
fn post_multipart(
    addr: &str,
    path: &str,
    cookie: &str,
    field: &str,
    filename: &str,
    content: &str,
) -> HttpResponse {
    let boundary = "----referencebattery7f4b2c9e";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{filename}\"\r\nContent-Type: text/csv\r\n\r\n{content}\r\n--{boundary}--\r\n"
    );
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: l\r\nContent-Type: multipart/form-data; boundary={boundary}\r\nContent-Length: {}\r\nCookie: {cookie}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    send(addr, req.as_bytes())
}
