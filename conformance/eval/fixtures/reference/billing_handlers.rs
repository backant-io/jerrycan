//! Handlers for `billing` — the Stripe webhook. Verifies the signature over the
//! RAW request body (HMAC-SHA256 hex), so it must read the body as bytes, never
//! re-serialize it (a re-encode would change the bytes and break the signature).
use jerrycan::auth::webhook::verify_sha256_hex;
use jerrycan::prelude::*;

/// The signing secret. Read from the environment in production; a fixed default
/// keeps the slice hermetic for the live battery (which signs with the same
/// value). NEVER a literal secret in real code — this is a reference slice.
fn webhook_secret() -> Vec<u8> {
    std::env::var("STRIPE_WEBHOOK_SECRET")
        .unwrap_or_else(|_| "whsec_reference_reference_secret".to_string())
        .into_bytes()
}

/// POST /webhook — verify `Stripe-Signature` against the raw body.
/// No signature header at all → 200 (an unsigned ping / the generated probe).
/// Header present and valid → 200; header present but wrong/forged → 400.
pub(crate) async fn stripe_webhook(headers: Headers, body: RawBody) -> Result<Json<serde_json::Value>> {
    let Some(signature) = headers.get("stripe-signature") else {
        return Ok(Json(serde_json::json!({ "received": true, "verified": false })));
    };
    if verify_sha256_hex(&webhook_secret(), &body.0, signature) {
        Ok(Json(serde_json::json!({ "received": true, "verified": true })))
    } else {
        Err(Error::bad_request("Stripe signature is missing or invalid"))
    }
}
