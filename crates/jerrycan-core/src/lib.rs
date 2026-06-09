//! Core framework of the jerrycan platform: routing, extractors, dependency
//! injection, middleware. See https://jerrycan.cc
#![forbid(unsafe_code)]

pub mod error;
pub mod response;

pub use error::{Error, Result};
pub use response::{Created, IntoResponse, Json, NoContent, Response};
