//! Core framework of the jerrycan platform: routing, extractors, dependency
//! injection, middleware. See https://jerrycan.cc
#![forbid(unsafe_code)]

pub mod error;

pub use error::{Error, Result};
