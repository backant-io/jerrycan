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
        code: "JC0422",
        title: "unprocessable entity",
        cause: "the JSON body failed to parse, or Valid<T> found violations",
        fix: "fix the body to match the schema; read the details array",
        doc: "jerrycan docs validation",
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
