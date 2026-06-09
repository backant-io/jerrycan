//! Proc-macro sugar for the jerrycan framework.
#![forbid(unsafe_code)]

use proc_macro::TokenStream;

/// `#[jerrycan::main]` — boots the async runtime around `async fn main`.
/// Today it delegates to `#[tokio::main]`; the app must (and generated apps
/// do) depend on tokio directly.
#[proc_macro_attribute]
pub fn main(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let wrapped = format!("#[::tokio::main]\n{item}");
    wrapped
        .parse()
        .expect("jerrycan::main: item must be a valid async fn")
}
