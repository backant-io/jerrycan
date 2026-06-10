//! tools/call dispatch — the MCP twins of the CLI commands.

use serde_json::{Value, json};

pub fn dispatch(name: &str, args: &Value) -> (bool, Value) {
    match name {
        "jerrycan_docs_search" => {
            let query = args["query"].as_str().unwrap_or("");
            let limit = args["limit"].as_u64().unwrap_or(5) as usize;
            (
                false,
                json!({ "results": super::docsidx::search(query, limit) }),
            )
        }
        "jerrycan_docs_get" => {
            let page = args["page"].as_str().unwrap_or("");
            match super::docsidx::get(page, args["anchor"].as_str()) {
                Some(md) => (false, json!({ "markdown": md })),
                None => (
                    true,
                    json!({ "error": format!("unknown docs page `{page}`") }),
                ),
            }
        }
        other => (
            true,
            json!({ "error": format!("tool `{other}` lands in Task 16") }),
        ),
    }
}
