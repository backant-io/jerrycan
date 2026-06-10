# Validation

## Purpose
`jerrycan::validate` checks payload invariants AFTER extraction and turns
violations into structured `422 JC0422` responses (machine-readable `details`).
Enable with the design dependency `"validate"` (also serves `/openapi.json`).

## Signature
```rust
# use jerrycan::prelude::*;
use jerrycan::validate::{Valid, Validate, Violation};
# use serde::Deserialize;

#[derive(Deserialize)]
struct NewNote { text: String }

impl Validate for NewNote {
    fn validate(&self) -> Result<(), Vec<Violation>> {
        if self.text.trim().is_empty() {
            return Err(vec![Violation::new("text", "must not be empty")]);
        }
        Ok(())
    }
}

async fn create(Valid(Json(note)): Valid<Json<NewNote>>) -> Result<Created<String>> {
    Ok(Created(note.text))
}
# let _ = create;
```

## Minimal example
```rust
# use jerrycan::prelude::*;
# use jerrycan::validate::{Valid, Validate, Violation};
# use serde::{Deserialize, Serialize};
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
#[derive(Deserialize, Serialize)]
struct NewNote { text: String }
impl Validate for NewNote {
    fn validate(&self) -> Result<(), Vec<Violation>> {
        if self.text.trim().is_empty() {
            return Err(vec![Violation::new("text", "must not be empty")]);
        }
        Ok(())
    }
}
async fn create(Valid(Json(n)): Valid<Json<NewNote>>) -> Json<NewNote> { Json(n) }

let t = App::new().route("/notes", post(create)).into_test();
let res = t.post_json("/notes", &NewNote { text: "  ".into() }).await;
assert_eq!(res.status(), jerrycan::http::StatusCode::UNPROCESSABLE_ENTITY);
let body: serde_json::Value = res.json();
assert_eq!(body["details"][0]["field"], "text");
# }); }
```

## Variations
- `OpenApi::new(include_str!("../../../openapi.json"))` as an extension serves the
  platform-generated document at `GET /openapi.json` (generated apps wire this
  when the design lists `"validate"`).
- Rule-attribute `derive(Validate)` is a contract-v1 candidate — today you
  implement the trait by hand (the design schema has no field constraints yet).

## Errors you'll hit
- `422 JC0422` with `details: [{field, message}]` — exactly what `Valid<T>`
  produces from your `Validate` impl. Plain `Json<T>` parse failures stay
  `422 JC0422` WITHOUT details.

## Anti-patterns
- Don't validate inside handlers — `Valid<…>` keeps the contract in the
  signature where agents and reviewers can see it.
- Don't return ad-hoc 400s for invariant failures; violations belong in
  `details` where tooling can read them.
