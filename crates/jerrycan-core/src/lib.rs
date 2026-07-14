//! Core framework of the jerrycan platform: routing, extractors, dependency
//! injection, middleware. Generated apps import this through the `jerrycan`
//! facade crate — see <https://jerrycan.cc>
#![forbid(unsafe_code)]

pub mod app;
pub mod clock;
pub mod cors;
pub mod dep;
pub mod error;
pub mod extract;
pub mod handler;
pub mod middleware;
pub mod module;
pub mod multipart;
pub mod response;
pub mod router;
mod serve;
pub mod test_client;

pub use app::{App, BuiltApp, Extension};
pub use clock::Clock;
pub use cors::{CorsConfig, CorsOrigins};
pub use dep::{Dep, TaskContext};
pub use error::{Error, Result};
pub use extract::{FromRequest, Headers, Path, Query, RawBody, RequestCtx};
pub use handler::Handler;
pub use middleware::{Middleware, MiddlewareFuture, Next};
pub use module::Module;
pub use multipart::Multipart;
pub use response::{
    BodyError, BodySender, Created, IntoResponse, JcBody, Json, NoContent, Redirect, Response,
    StreamBody,
};
pub use router::{MethodRouter, delete, get, patch, post, put};
pub use test_client::{TestApp, TestPart, TestResponse};

/// Re-exported so apps and tests never add `http` to their own Cargo.toml.
pub use http;

/// Re-exported so agent-owned app code (e.g. the error-body mapper in
/// `crates/app/src/errors.rs`) can build `serde_json::Value` bodies without
/// adding a separate `serde_json` dependency to the app crate.
pub use serde_json;

/// Re-exported for webhook recipes that parse `application/x-www-form-urlencoded`
/// bodies (e.g. Twilio's signed form params) without a separate dependency.
pub use serde_urlencoded;

/// One import for generated code: `use jerrycan::prelude::*;`
pub mod prelude {
    pub use crate::{
        App, Clock, CorsConfig, CorsOrigins, Created, Dep, Error, Extension, Headers, IntoResponse,
        Json, Middleware, MiddlewareFuture, Module, Multipart, Next, NoContent, Path, Query,
        RawBody, Redirect, RequestCtx, Result, StreamBody, TestApp, TestPart, delete, get, patch,
        post, put,
    };
}
