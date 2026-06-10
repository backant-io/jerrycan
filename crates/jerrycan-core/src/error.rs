//! jerrycan's single error type. Every error carries a stable `code` (JC####)
//! that maps to a documentation anchor — the error-driven-docs contract (spec §8).

use http::StatusCode;
use std::fmt;

/// Convenience alias used across jerrycan and generated apps.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// The one error type of the framework (spec §4.1 "Errors").
///
/// Production responses render only `code` + `message` as JSON; internals
/// (sources, backtraces) are for logs — enforced in Phase 1's observe layer.
#[derive(Debug)]
pub struct Error {
    status: StatusCode,
    code: &'static str,
    message: String,
    details: Option<serde_json::Value>,
}

impl Error {
    /// Build an error with an explicit status and stable code.
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            details: None,
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "JC0400", message)
    }
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "JC0404", "not found")
    }
    pub fn method_not_allowed() -> Self {
        Self::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "JC0405",
            "method not allowed",
        )
    }
    pub fn payload_too_large() -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, "JC0413", "payload too large")
    }
    pub fn unprocessable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, "JC0422", message)
    }
    /// The handler exceeded the configured time budget (spec §4.4 timeouts).
    pub fn handler_timeout() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "JC0503",
            "handler timed out",
        )
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "JC0500", message)
    }
    /// A handler or dependency asked for a type no provider supplies (spec §4.3).
    pub fn missing_dependency(type_name: &str) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "JC1001",
            format!("no provider registered for dependency `{type_name}`"),
        )
    }

    /// Dependency factories recursed past the depth limit (cycle, or absurd chain).
    pub fn dependency_cycle() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "JC1002",
            "dependency cycle or chain too deep",
        )
    }

    /// Attach machine-readable detail (e.g. validation violations). Rendered
    /// as a `details` key in the response body; absent otherwise.
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn details(&self) -> Option<&serde_json::Value> {
        self.details.as_ref()
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }
    pub fn code(&self) -> &'static str {
        self.code
    }
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_carry_status_and_stable_code() {
        assert_eq!(Error::not_found().status(), StatusCode::NOT_FOUND);
        assert_eq!(Error::not_found().code(), "JC0404");
        assert_eq!(Error::method_not_allowed().code(), "JC0405");
        assert_eq!(Error::bad_request("nope").status(), StatusCode::BAD_REQUEST);
        assert_eq!(Error::payload_too_large().code(), "JC0413");
        assert_eq!(Error::unprocessable("bad field").code(), "JC0422");
        assert_eq!(
            Error::internal("boom").status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        let e = Error::missing_dependency("app::Db");
        assert_eq!(e.code(), "JC1001");
        assert_eq!(e.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(e.message().contains("app::Db"));
        assert_eq!(Error::dependency_cycle().code(), "JC1002");
        assert_eq!(Error::handler_timeout().code(), "JC0503");
        assert_eq!(
            Error::handler_timeout().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[test]
    fn details_attach_and_default_to_none() {
        let plain = Error::unprocessable("validation failed");
        assert!(plain.details().is_none());
        let detailed = Error::unprocessable("validation failed").with_details(
            serde_json::json!([{ "field": "title", "message": "must not be empty" }]),
        );
        assert!(detailed.details().unwrap().is_array());
    }

    #[test]
    fn display_includes_code_and_message() {
        let e = Error::bad_request("missing body");
        assert_eq!(format!("{e}"), "JC0400: missing body");
    }
}
