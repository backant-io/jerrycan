//! Proc-macro sugar for the jerrycan framework.
#![forbid(unsafe_code)]

use proc_macro::TokenStream;

/// `#[jerrycan::main]` — boots the async runtime around `async fn main`.
/// Delegates to `#[tokio::main]`; the app must (and generated apps do) depend
/// on tokio directly. The user's tokens pass through UNCHANGED, preserving
/// their spans so compiler diagnostics point at the user's code, not at this
/// attribute.
#[proc_macro_attribute]
pub fn main(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut out: TokenStream = "#[::tokio::main]"
        .parse()
        .expect("static attribute tokens always parse");
    out.extend(item); // original tokens, original spans
    out
}
