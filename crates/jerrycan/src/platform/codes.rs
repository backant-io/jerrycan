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
        assert_eq!(lookup("JC0530").unwrap().title, "realtime requires postgres");
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
