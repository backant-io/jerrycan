//! Core framework of the jerrycan platform: routing, extractors, dependency
//! injection, middleware. See https://jerrycan.cc
#![forbid(unsafe_code)]

pub mod app;
pub mod dep;
pub mod error;
pub mod extract;
pub mod handler;
pub mod middleware;
pub mod module;
pub mod response;
pub mod router;

pub use app::{App, BuiltApp};
pub use dep::Dep;
pub use error::{Error, Result};
pub use extract::{FromRequest, Path, Query, RequestCtx};
pub use handler::Handler;
pub use middleware::{Middleware, MiddlewareFuture, Next};
pub use module::Module;
pub use response::{Created, IntoResponse, Json, NoContent, Response};
pub use router::{MethodRouter, delete, get, patch, post, put};
