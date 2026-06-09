//! Core framework of the jerrycan platform: routing, extractors, dependency
//! injection, middleware. See https://jerrycan.cc
#![forbid(unsafe_code)]

pub mod dep;
pub mod error;
pub mod extract;
pub mod response;

pub use dep::Dep;
pub use error::{Error, Result};
pub use extract::{FromRequest, Path, Query, RequestCtx};
pub use response::{Created, IntoResponse, Json, NoContent, Response};
