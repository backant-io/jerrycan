//! The single registry of stable diagnostic codes. `jerrycan explain` reads it;
//! a completeness test fails if any code emitted in source is missing here.

/// One diagnostic code's human explanation.
pub struct CodeInfo {
    pub code: &'static str,
    pub title: &'static str,
    pub cause: &'static str,
    pub fix: &'static str,
    pub doc: &'static str,
}

/// Every JC#### (framework runtime) and JL#### (jerrycan generation lint) code.
pub const REGISTRY: &[CodeInfo] = &[
    CodeInfo {
        code: "JC0400",
        title: "bad request",
        cause: "a path parameter or query string failed to parse, or the path had a malformed percent-encoding",
        fix: "send well-formed input; check the route's parameter types",
        doc: "jerrycan docs errors",
    },
    CodeInfo {
        code: "JC0401",
        title: "authentication required",
        cause: "no valid session cookie or bearer token was presented",
        fix: "log in (Session) or send Authorization: Bearer <jwt>",
        doc: "jerrycan docs auth",
    },
    CodeInfo {
        code: "JC0403",
        title: "forbidden",
        cause: "authenticated, but require_role rejected the user's role",
        fix: "use an account with the required role",
        doc: "jerrycan docs auth",
    },
    CodeInfo {
        code: "JC0404",
        title: "not found",
        cause: "no route matched the path, or a handler returned Error::not_found()",
        fix: "check the path; confirm the resource exists",
        doc: "jerrycan docs app",
    },
    CodeInfo {
        code: "JC0405",
        title: "method not allowed",
        cause: "the path exists but not for this HTTP method",
        fix: "use a method the route defines",
        doc: "jerrycan docs app",
    },
    CodeInfo {
        code: "JC0408",
        title: "request timeout",
        cause: "the request body was not received within the read budget",
        fix: "send the body promptly; raise body_read_timeout if legitimate",
        doc: "jerrycan docs app",
    },
    CodeInfo {
        code: "JC0409",
        title: "conflict",
        cause: "the write violates a unique key (e.g. a re-POSTed id)",
        fix: "use a fresh key, or treat as already-created (idempotent retry)",
        doc: "jerrycan docs database",
    },
    CodeInfo {
        code: "JC0413",
        title: "payload too large",
        cause: "the request body exceeded the size limit (default 1 MiB)",
        fix: "send a smaller body; raise the limit explicitly if needed",
        doc: "jerrycan docs app",
    },
    CodeInfo {
        code: "JC0415",
        title: "unsupported media type",
        cause: "the request's content type is not what the endpoint consumes: Multipart requires multipart/form-data with a boundary, and a storage bucket upload must match the bucket's allowed_mime allowlist",
        fix: "send the content type the endpoint declares; for uploads, multipart/form-data with a valid boundary parameter, or a Content-Type inside the bucket's allowed_mime list",
        doc: "jerrycan docs extractors",
    },
    CodeInfo {
        code: "JC0422",
        title: "unprocessable entity",
        cause: "the JSON body failed to parse, or Valid<T> found violations",
        fix: "fix the body to match the schema; read the details array",
        doc: "jerrycan docs validation",
    },
    CodeInfo {
        code: "JC0429",
        title: "too many requests",
        cause: "the client exceeded the configured rate limit for its identity (api-key → user → IP) in the current fixed window",
        fix: "slow down and retry after the Retry-After delay; raise the limit in the rate-limit extension config if the traffic is legitimate",
        doc: "jerrycan docs middleware",
    },
    CodeInfo {
        code: "JC0500",
        title: "internal error",
        cause: "an unexpected server-side failure (or a handler panicked)",
        fix: "check server logs; the cause is logged, never sent to the client",
        doc: "jerrycan docs errors",
    },
    CodeInfo {
        code: "JC0503",
        title: "handler timeout",
        cause: "the request exceeded the per-request handler budget (default 30s)",
        fix: "make the handler faster or raise handler_timeout",
        doc: "jerrycan docs app",
    },
    CodeInfo {
        code: "JC0510",
        title: "database error",
        cause: "a jerrycan-db query/connection failed",
        fix: "check JERRYCAN_DATABASE_URL and migrations; the sqlx detail is on stderr",
        doc: "jerrycan docs database",
    },
    CodeInfo {
        code: "JC0520",
        title: "schema contract is stale",
        cause: "schema.json does not match the schema derived from the module migrations",
        fix: "run jerrycan schema --write and commit the result",
        doc: "jerrycan docs database",
    },
    CodeInfo {
        code: "JC0521",
        title: "job failed",
        cause: "a background job returned an error and (after its retries) was moved to the dead-letter table, or failed irrecoverably",
        fix: "inspect the dead-letter table and the operator logs; fix the job handler and requeue the dead-lettered job",
        doc: "jerrycan docs jobs",
    },
    CodeInfo {
        code: "JC1001",
        title: "missing dependency",
        cause: "a handler asked for a Dep<T> with no registered provider",
        fix: "provide(value) or provide_dep(factory) on the app or module",
        doc: "jerrycan docs dependencies",
    },
    CodeInfo {
        code: "JC1002",
        title: "dependency cycle",
        cause: "dependency factories recursed past the depth limit (cycle or absurd chain)",
        fix: "break the cycle in your provide_dep graph",
        doc: "jerrycan docs dependencies",
    },
    CodeInfo {
        code: "JC1003",
        title: "dependency requires an HTTP request",
        cause: "a dependency factory used an HTTP extractor (Json/Path/Query/Headers) but was resolved in a task context (background job, startup)",
        fix: "restructure the factory to take only Dep<T> arguments, or resolve it inside a request",
        doc: "jerrycan docs dependencies",
    },
    CodeInfo {
        code: "JL0001",
        title: "leaky route crate",
        cause: "a route crate's lib.rs exports more than module()",
        fix: "make it pub(crate), or move shared types to the shared crate",
        doc: "jerrycan docs modules",
    },
    CodeInfo {
        code: "JL0002",
        title: "missing handler",
        cause: "a design endpoint has no matching handler fn",
        fix: "add the handler with the operation_id name, or fix the design",
        doc: "jerrycan docs modules",
    },
    CodeInfo {
        code: "JL0003",
        title: "generated drift",
        cause: "a tool-owned generated file was hand-edited or the design changed without regenerating",
        fix: "re-run jerrycan generate; never hand-edit GENERATED files",
        doc: "jerrycan docs app",
    },
    CodeInfo {
        code: "JL0004",
        title: "unguarded mutation",
        cause: "an auth design has a mutating route with no auth guard",
        fix: "set auth_required: true or required_roles on the endpoint",
        doc: "jerrycan docs auth",
    },
    CodeInfo {
        code: "JL0006",
        title: "cross-tenant data access",
        cause: "a handler for a tenant-owned entity used an unscoped repo method (all/get/remove), so it can read or delete another tenant's rows",
        fix: "call the tenant-scoped accessor (all_for/get_for/remove_for) with the current tenant's id",
        doc: "jerrycan docs database",
    },
    CodeInfo {
        code: "JL0007",
        title: "request-boundary escape",
        cause: "agent-owned module code calls into process/filesystem/network APIs directly — outside the framework's request boundary and threat model",
        fix: "use framework extensions for I/O; if genuinely intended, append `// jerrycan:allow JL0007` to the line",
        doc: "jerrycan docs errors",
    },
    CodeInfo {
        code: "JL0008",
        title: "tenant-owned handler could not be scanned for scoping",
        cause: "JL0006 must read and parse each tenant-owned module's handlers.rs to verify it uses the scoped accessors, but this file is missing, unreadable, or not valid Rust — so scoping could not be checked and an unscoped cross-tenant call could pass unseen",
        fix: "ensure the handler file exists and compiles (run `cargo check`); a scaffold is generated parseable — if you hand-edited it, fix the syntax so `jerrycan check` can verify tenant scoping",
        doc: "jerrycan docs database",
    },
    CodeInfo {
        code: "JC0540",
        title: "tenant entity is the auth identity",
        cause: "the design's tenancy.entity names the auth identity entity — its derived foreign key column is `user_id`, the same column the generated membership table already uses for the authenticated user, so a user cannot be their own tenant org and the auth_0001 migration would fail with `duplicate column name: user_id`",
        fix: "for per-user data, drop the tenancy block and give each owned entity a belongs_to the identity plus tenant-scoped guard methods (all_for/get_for); for orgs/teams, point tenancy.entity at a SEPARATE tenant entity (e.g. Org or Workspace) that users hold a membership in",
        doc: "jerrycan docs tenancy",
    },
    CodeInfo {
        code: "JC0541",
        title: "entity name shadows a generated request DTO",
        cause: "an entity is literally named `{X}Request` while another entity `X` omits a server-owned field (an identity fk, a `default`, or a path-redundant parent fk) and so generates a `{X}Request` DTO — the Rust struct and the OpenAPI component would be defined twice, a compile error plus a silently clobbered schema",
        fix: "rename the `{X}Request` entity (e.g. `{X}Payload` or `{X}Submission`); the `{X}Request` name is reserved for the generated request DTO of entity `X`",
        doc: "jerrycan docs validation",
    },
    CodeInfo {
        code: "JC0542",
        title: "conflicting path parameters across sibling routes",
        cause: "two routes reach the same path segment position through an identical prefix but name that position's `{param}` differently (e.g. `/tickets/{id}` and `/tickets/{ticket_id}/comments`) — the router keys each position by a SINGLE parameter name, so registering both aborts `App::build` at startup with JC0500 `conflicting path parameters` (after a clean scaffold, mid-test); with `tenancy`, the implicit member-management routes (`/{tenant_fk}/members`, `/{tenant_fk}/members/{user_id}`, issue #107) join this check, so a tenant-module endpoint with a custom param name, or one occupying a reserved member path, conflicts the same way",
        fix: "give the shared segment ONE parameter name in every route that reaches it (rename `{ticket_id}`→`{id}` or vice versa), or restructure the nesting so the position is not shared (mount the diverging routes under distinct static prefixes)",
        doc: "jerrycan docs app",
    },
    CodeInfo {
        code: "JC0543",
        title: "enum value is not an identifier",
        cause: "a string field's enum `values` entry contains a character outside ^[A-Za-z0-9_-]+$ — values are interpolated UNESCAPED into generated Rust (the deserialize allow-list, the 422 error text, and the test fixtures), so a quote or backslash emits a crate that fails to compile far from the design (other non-identifier characters are rejected for the same interpolation-safety rule)",
        fix: "use identifier-shaped enum values (ASCII letters, digits, `_`, `-`); keep any human display label out of the stored value",
        doc: "jerrycan docs validation",
    },
    CodeInfo {
        code: "JC0544",
        title: "nested-entity create route cannot supply its path-owned foreign key",
        cause: "an entity has a parent foreign key another route supplies from a path parameter, so the shared per-entity request DTO drops it for EVERY create of the entity — but this body-carrying create/update route's own path has no matching `{param}`, so the NOT-NULL column can be set from neither the body nor the path and the route is un-implementable",
        fix: "add the parent's `{fk}` path parameter to this route (mount it under the parent), or split the entity so the standalone route uses its own request body that keeps the fk",
        doc: "jerrycan docs validation",
    },
    CodeInfo {
        code: "JC0545",
        title: "entity reaches the tenant through more than one path",
        cause: "an entity has two or more distinct `belongs_to` chains that each reach the tenant entity (a diamond graph), so jerrycan cannot decide which chain defines tenant ownership — guessing would scope reads/writes to the wrong tenant and re-open the cross-tenant leak",
        fix: "collapse the entity's tenant ownership to a SINGLE `belongs_to` path (drop the redundant parent, or split the entity), so exactly one chain reaches the tenant",
        doc: "jerrycan docs database",
    },
    CodeInfo {
        code: "JC0546",
        title: "entity name collides with a prelude re-export",
        cause: "an entity is named the same as an identifier re-exported by `jerrycan::prelude` (e.g. `Module`, `Error`, `Json`) — generated modules write `use jerrycan::prelude::*;` next to `use super::model::*;`, so the entity's generated `struct` and the prelude item are two glob imports of the same name, and every reference is E0659 `... is ambiguous`; the scaffolded crate does not compile",
        fix: "rename the entity so its name is not a reserved prelude identifier (e.g. `{Name}Record` or a domain-specific name)",
        doc: "jerrycan docs validation",
    },
    CodeInfo {
        code: "JC0547",
        title: "realtime changes on a transitively tenant-owned entity",
        cause: "a realtime `changes` entity reaches the tenant only through an intermediate parent (a grandchild chain like Contact -> Account -> Org), so its row image carries no tenant key column — change events could not be tenant-scoped and every tenant's rows would broadcast to every authenticated principal",
        fix: "the changes entity must be the tenant itself or a DIRECT child of it: flatten the relationship (give the entity its own belongs_to the tenant) or drop it from `changes`",
        doc: "jerrycan docs realtime",
    },
    CodeInfo {
        code: "JC0548",
        title: "invalid tenancy member_roles",
        cause: "`tenancy.member_roles` is empty, repeats a role, or contains a role outside ^[A-Za-z0-9_-]+$ — `member_roles[0]` is the admin role the generated member-management surface gates on, the list becomes the generated MEMBER_ROLES allow-list and the OpenAPI `role` enum, and role names are interpolated UNESCAPED into generated Rust string literals (the MEMBER_ROLES const, the membership seed, `require_role` gates), so an empty or duplicated list breaks the admin-role convention and a quote or backslash emits a crate that fails to compile",
        fix: "declare a non-empty, duplicate-free member_roles list of identifier-shaped names (letters, digits, `_`, `-`), admin role first (e.g. [\"owner\", \"member\"])",
        doc: "jerrycan docs tenancy",
    },
    CodeInfo {
        code: "JC0530",
        title: "realtime requires postgres",
        cause: "the design declares realtime changes but the app is running on sqlite",
        fix: "point JERRYCAN_DATABASE_URL at a Postgres database (broadcast/presence channels work without it; changes channels need Postgres)",
        doc: "jerrycan docs realtime",
    },
    CodeInfo {
        code: "JC0531",
        title: "realtime replication unavailable",
        cause: "wal_level is not 'logical' or the role lacks REPLICATION, so changes run on the trigger + LISTEN/NOTIFY fallback (identical client behavior, weaker delivery guarantee)",
        fix: "set wal_level=logical and grant REPLICATION to the app role, then restart Postgres — realtime upgrades itself on next start",
        doc: "jerrycan docs realtime",
    },
];

/// Look up a code, case-insensitively.
pub fn lookup(code: &str) -> Option<&'static CodeInfo> {
    let upper = code.to_uppercase();
    REGISTRY.iter().find(|c| c.code == upper)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_is_case_insensitive() {
        assert_eq!(lookup("jc0404").unwrap().code, "JC0404");
        assert_eq!(lookup("JC0404").unwrap().code, "JC0404");
        // Built at runtime so this source file holds no code-shaped literal the
        // completeness walk would mistake for an emitted code.
        let absent = format!("JC{}", 9999);
        assert!(lookup(&absent).is_none());
    }

    #[test]
    fn realtime_codes_are_registered() {
        assert_eq!(
            lookup("JC0530").unwrap().title,
            "realtime requires postgres"
        );
        assert_eq!(
            lookup("JC0531").unwrap().title,
            "realtime replication unavailable"
        );
    }

    #[test]
    fn jc0415_covers_bucket_mime_allowlists() {
        // WHY: `jerrycan explain JC0415` is the agent's first stop when a
        // generated bucket rejects an upload — the registry must name the
        // allowlist cause, not just the Multipart boundary case.
        let info = lookup("JC0415").unwrap();
        assert!(info.cause.contains("allowed_mime"), "cause: {}", info.cause);
    }

    #[test]
    fn jc0540_explains_the_tenant_identity_conflict() {
        // WHY: JC0540 is the agent's stop after the CLI rejects a design whose
        // tenancy.entity is the auth identity — the registry must state the cause
        // ("a user cannot be their own tenant org") and BOTH fixes.
        let info = lookup("JC0540").unwrap();
        assert!(
            info.cause.contains("auth identity") || info.cause.contains("own tenant"),
            "cause: {}",
            info.cause
        );
        assert!(
            info.fix.contains("belongs_to") && info.fix.contains("tenant entity"),
            "fix must name both remedies: {}",
            info.fix
        );
    }

    #[test]
    fn design_time_codes_542_543_544_are_registered_and_name_their_remedies() {
        // WHY: JC0542/JC0543/JC0544 are the P-A validator's design-time fail-loud
        // codes (#65/#54/#60). `jerrycan explain <code>` reads this registry, so
        // each must be present and each explanation must name the concrete fix(es).
        for code in ["JC0542", "JC0543", "JC0544"] {
            let info = lookup(code).unwrap_or_else(|| panic!("{code} must be registered"));
            assert!(
                !info.title.is_empty() && !info.cause.is_empty() && !info.fix.is_empty(),
                "{code} needs a full explanation"
            );
        }
        // JC0542 names BOTH remedies: unify the name, or restructure the nesting.
        let router = lookup("JC0542").unwrap();
        assert!(
            router.fix.contains("ONE parameter name") && router.fix.contains("restructure"),
            "JC0542 must name both remedies: {}",
            router.fix
        );
        // JC0544 names BOTH remedies: add the path param, or split the entity.
        let dual = lookup("JC0544").unwrap();
        assert!(
            dual.fix.contains("path parameter") && dual.fix.contains("split"),
            "JC0544 must name both remedies: {}",
            dual.fix
        );
    }

    #[test]
    fn jc0548_names_all_three_member_roles_failure_modes() {
        // WHY: JC0548 is the agent's stop after `check` rejects a tenancy design's
        // member_roles (#107) — the registry must name all three ways to be wrong
        // (empty / duplicated / non-identifier), the admin-role convention the
        // list backs, and the unescaped-interpolation reason for the charset.
        let info = lookup("JC0548").unwrap();
        assert!(
            info.cause.contains("empty")
                && info.cause.contains("repeats")
                && info.cause.contains("[A-Za-z0-9_-]"),
            "cause must name all three failure modes: {}",
            info.cause
        );
        assert!(
            info.cause.contains("member_roles[0]") && info.cause.contains("UNESCAPED"),
            "cause must state the admin convention and the interpolation risk: {}",
            info.cause
        );
        assert!(
            info.fix.contains("non-empty") && info.fix.contains("admin role first"),
            "fix must state the required shape: {}",
            info.fix
        );
    }

    #[test]
    fn jc0547_names_the_transitive_changes_leak_and_both_remedies() {
        // WHY: JC0547 converts the transitive-changes silent cross-tenant
        // broadcast leak (#102's realtime facet) into a design-time refusal —
        // `jerrycan explain JC0547` must state the cause (no tenant key in the
        // row image) and name BOTH remedies (flatten, or drop from `changes`).
        let info = lookup("JC0547").unwrap();
        assert!(
            info.cause.contains("tenant key") && info.cause.contains("broadcast"),
            "cause must name the missing row-image tenant key and the leak: {}",
            info.cause
        );
        assert!(
            info.fix.contains("flatten") && info.fix.contains("drop"),
            "fix must name both remedies: {}",
            info.fix
        );
    }

    #[test]
    fn every_emitted_code_is_in_the_registry() {
        // Grep the workspace source for JC####/JL#### string literals and assert
        // each is registered. This is the "no orphan codes" guard. We walk only
        // each crate's src/ (not tests/): codes that appear solely in test
        // fixtures (e.g. a user-authored 409 ErrorCase in testgen.rs) are example
        // text, not framework-emitted diagnostics.
        use std::collections::BTreeSet;
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap()
            .join("crates");
        let mut found = BTreeSet::new();
        fn walk(dir: &std::path::Path, found: &mut BTreeSet<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if matches!(name, "target" | ".git" | "fuzz" | "flask" | "werkzeug") {
                        continue;
                    }
                    walk(&p, found);
                } else if p.extension().is_some_and(|x| x == "rs")
                    && let Ok(s) = std::fs::read_to_string(&p)
                {
                    for cap in find_codes(&s) {
                        found.insert(cap);
                    }
                }
            }
        }
        // Only each crate's src/ tree — never its tests/.
        let Ok(entries) = std::fs::read_dir(&crates) else {
            panic!("cannot read {}", crates.display());
        };
        for e in entries.flatten() {
            let src = e.path().join("src");
            if src.is_dir() {
                walk(&src, &mut found);
            }
        }
        let registered: BTreeSet<String> = REGISTRY.iter().map(|c| c.code.to_string()).collect();
        let orphans: Vec<&String> = found.iter().filter(|c| !registered.contains(*c)).collect();
        assert!(
            orphans.is_empty(),
            "codes emitted in source but missing from the registry: {orphans:?}"
        );
    }

    /// Extract `JC####` / `JL####` tokens from a source string.
    fn find_codes(s: &str) -> Vec<String> {
        let bytes = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i + 6 <= bytes.len() {
            let w = &bytes[i..i + 6];
            let is_code = (w[0] == b'J')
                && (w[1] == b'C' || w[1] == b'L')
                && w[2..].iter().all(u8::is_ascii_digit);
            if is_code {
                // ensure not part of a longer alnum run
                let before_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
                let after_ok = i + 6 == bytes.len() || !bytes[i + 6].is_ascii_alphanumeric();
                if before_ok && after_ok {
                    out.push(String::from_utf8_lossy(w).to_string());
                }
            }
            i += 1;
        }
        out
    }
}
