# Storage

Buckets are modeled in `design.json` (contract v2); the generator emits guarded
per-bucket endpoints; object metadata lives in the `storage_objects` table;
bytes live in a pluggable blob store.

## Declaring buckets (contract v2)

```json
{
  "contract_version": 2,
  "storage": {
    "buckets": [
      { "name": "avatars", "visibility": "public", "owner": "User",
        "max_size": "5MB", "allowed_mime": ["image/*"] },
      { "name": "invoices", "visibility": "private", "owner": "Org",
        "owner_prefix": true, "max_size": "20MB",
        "write_roles": ["admin"] }
    ]
  }
}
```

- `visibility: public` — unauthenticated `GET` reads; mutations are ALWAYS guarded.
- `owner` — the tenancy entity makes the bucket tenant-owned (Tenant-guard
  scoped); any other entity stamps the session user id.
- `owner_prefix: true` — keys stored as `{owner_id}/…`, prefix-asserted on every
  access (the Supabase folder-per-user pattern).
- `write_roles` — on a TENANT-scoped bucket, the member roles allowed to write
  (upload/delete). A tenant bucket stamps `owner_id = tenant.id()`, so every
  member is "the owner"; without `write_roles` any member — including a
  read-only role — can upload bytes and delete others' uploads (#132). List the
  write roles (each a declared `tenancy.member_roles`) and a non-write-role
  member gets `403` on upload/delete. Reads (download/list/sign) are never
  role-gated. Empty/absent = any member may write (the backward-compatible
  default). On a non-tenant bucket `write_roles` is meaningless and refused
  (`JC0556`).
- Storage requires the `db` dependency and an active auth model.

> **Per-owner key isolation (`owner_prefix`).** An OWNED bucket (tenant- or
> user-scoped) shares ONE key namespace across all owners unless
> `owner_prefix: true` is set: a key like `report.pdf` is a single global path,
> so one owner can learn of or squat another owner's keys (the #133 cross-owner
> key oracle). Set `owner_prefix: true` for per-owner key isolation — keys are
> stored under `{owner_id}/…` and asserted on every access, so owners cannot
> observe or collide on each other's keys.

## Generated endpoints (per bucket `<b>`)

Buckets mount under `storage.base_path` (default `/storage`), so a bucket `<b>`
serves at `/storage/<b>` — clear of your module mounts (a `media` bucket no
longer shadows a `/media` module). Set `storage.base_path` (e.g. `/files`) to
change the prefix; the paths below show the default.

| Route | Behavior |
|---|---|
| `POST /storage/<b>?key=<path>` | upload a raw body; `Content-Type` is the mime (415 `JC0415` outside `allowed_mime`; over `max_size` is 413 `JC0413`; duplicate key is 409) |
| `GET /storage/<b>` | list (owner/tenant scoped; open when public) |
| `GET /storage/<b>/{id}` | download — emits `ETag` (sha256) + `Cache-Control`, plus `X-Content-Type-Options: nosniff` and `Content-Disposition: attachment` (uploader-controlled bytes never render on the app's origin; `<img src=…>` embedding still works — subresource loads ignore the disposition); private buckets also accept `?exp=…&sig=…` |
| `DELETE /storage/<b>/{id}` | delete row + bytes (scoped; foreign object = 404) |
| `POST /storage/<b>/{id}/sign?ttl=300` | a time-limited signed URL |

## Limitations (v1)

- Upload is `POST /storage/<b>?key=<path>` with a RAW request body only — there is no
  `multipart/form-data` parsing on bucket routes in v1. The request
  `Content-Type` header is the stored mime.
- Uploads are buffered whole in memory before they reach the blob store,
  bounded by the bucket's `max_size` (which is also the route's `body_limit`).
  Streaming uploads are a later enhancement.
- `max_size` units are binary: `KB`/`MB`/`GB` mean KiB/MiB/GiB — `"5MB"` is
  5 × 1024 × 1024 bytes.
- Dependencies: storage adds `quick-xml` to the workspace (S3 error/multipart
  XML parsing), and the companion lossless-migration work adds `bcrypt`
  (verify-only, for migrated password hashes) to `jerrycan-auth` — two new
  third-party crates in total, not one.

## Configuring the backend (env, zero-touch)

```text
JERRYCAN_STORAGE=local:/var/data                      # filesystem (default: local:./storage)
JERRYCAN_STORAGE=s3://bucket?region=eu-central-1      # AWS S3
JERRYCAN_STORAGE=s3://bucket?endpoint=https://…       # R2 / MinIO / Supabase S3
```

S3 credentials come from `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`; signed
URLs use `JERRYCAN_SECRET`. The packaged binary carries both backends — the env
var alone switches them.

## Using the service directly

Generated handlers take `Dep<Storage>`; you can also call the service yourself.
`put_object` validates the key, enforces `max_size`/`allowed_mime`, prepends the
owner prefix, and stamps the metadata row. `bytes` is re-exported as
`jerrycan::storage::bytes` so no separate dependency is needed:

```rust
use jerrycan::prelude::*;
use jerrycan::storage::{Bucket, Scope, Storage, bytes::Bytes};

const REPORTS: Bucket = Bucket {
    name: "reports",
    public: false,
    owner_prefix: false,
    max_size: 1024 * 1024,
    allowed_mime: &["application/pdf"],
    mount_prefix: "/storage",
};

async fn archive(storage: Dep<Storage>, db: Dep<jerrycan::db::Db>) -> Result<Json<String>> {
    let scope = Scope { owner_id: Some("1".into()), tenant_id: None };
    let meta = storage
        .put_object(&db, &REPORTS, &scope, "q3.pdf", "application/pdf", Bytes::from_static(b"%PDF-"))
        .await?;
    Ok(Json(meta.id))
}
# let _ = archive;
```
