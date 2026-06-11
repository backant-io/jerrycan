//! Core framework of the jerrycan platform: routing, extractors, dependency
//! injection, middleware. Generated apps import this through the `jerrycan`
//! facade crate — see <https://jerrycan.cc>
#![forbid(unsafe_code)]

pub mod app;
pub mod clock;
pub mod dep;
pub mod error;
pub mod extract;
pub mod handler;
pub mod middleware;
pub mod module;
pub mod response;
pub mod router;
mod serve;
pub mod test_client;

pub use app::{App, BuiltApp, Extension};
pub use clock::Clock;
pub use dep::{Dep, TaskContext};
pub use error::{Error, Result};
pub use extract::{FromRequest, Headers, Path, Query, RequestCtx};
pub use handler::Handler;
pub use middleware::{Middleware, MiddlewareFuture, Next};
pub use module::Module;
pub use response::{Created, IntoResponse, JcBody, Json, NoContent, Response};
pub use router::{MethodRouter, delete, get, patch, post, put};
pub use test_client::{TestApp, TestResponse};

/// Re-exported so apps and tests never add `http` to their own Cargo.toml.
pub use http;

/// One import for generated code: `use jerrycan::prelude::*;`
pub mod prelude {
    pub use crate::{
        App, Clock, Created, Dep, Error, Extension, Headers, IntoResponse, Json, Middleware,
        MiddlewareFuture, Module, Next, NoContent, Path, Query, RequestCtx, Result, TestApp,
        delete, get, patch, post, put,
    };
}
