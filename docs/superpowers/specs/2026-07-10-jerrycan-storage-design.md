# jerrycan-storage — Design Spec

**Date:** 2026-07-10
**Status:** Design **approved** (all open questions resolved at review 2026-07-10). Ready for implementation planning.
**Order:** first of three — storage → realtime → migrator.
**Contract impact:** introduces `design.json` **contract_version 2** (adds the top-level `storage` block; v0/v1 stay valid).
**Part of:** lossless Supabase migration program (see `jerrycan-supabase-migration-roadmap` memory). Designed with the migrator's needs as a hard input, even though it ships before the migrator.

---

## Goal

A first-class storage extension for jerrycan: **buckets are modeled in `design.json`**, the generator emits guarded upload/download/list/delete endpoints + signed URLs, object metadata lives in `jerrycan-db` (reusing the tenancy/guard machinery), and bytes live in a pluggable blob store (local filesystem for dev/self-host, S3-compatible for prod). End state: a Supabase Storage bucket + its access policy can be **mechanically translated** into a jerrycan bucket that behaves the same and is eval-gated.

## Non-goals (this spec)

- Realtime and the migrator itself (separate specs).
- Image transformation / on-the-fly resizing (Supabase has it; migrator reports it as a gap).
- Resumable/TUS uploads (v1 = simple + multipart; resumable is a later candidate).

## Architecture overview

Same shape as every existing extension crate (`jerrycan-ratelimit` is the reference):

- New crate `crates/jerrycan-storage/`.
- Facade feature `storage = ["dep:jerrycan-storage", "db"]` — **storage implies db** (like `jobs`), because object metadata is a DB table.
- Reserved `storage` dependency recognized in `design.rs` (`has_storage()`), surfaced in `facade_features()`, added to the reserved-name filter in `mounting.rs`.
- New generator module `storagegen.rs` emits per-bucket resources + tests, mirroring `jobsgen.rs`.

Split of responsibility (Rule 5): **modeling + access control + endpoint generation is deterministic → code/generator.** Nothing here needs the model at runtime.

## `design.json` contract (v2 addition): the `storage` block

New optional top-level object:

```json
"storage": {
  "buckets": [
    {
      "name": "avatars",
      "visibility": "public",
      "owner": "User",
      "max_size": "5MB",
      "allowed_mime": ["image/*"]
    },
    {
      "name": "invoices",
      "visibility": "private",
      "owner": "Org",
      "owner_prefix": true,
      "max_size": "20MB"
    }
  ]
}
```

Fields:

- `name` (required) — `^[a-z][a-z0-9-]*$`; becomes mount `/<name>`.
- `visibility` (required) — `public` | `private`. `public` ⇒ unauthenticated `GET` reads allowed (still metadata-tracked, cached); `private` ⇒ all endpoints guarded.
- `owner` (optional) — an entity name (`^[A-Z][A-Za-z0-9]*$`) that owns objects. If the owner entity `belongs_to` `tenancy.entity`, objects are **automatically tenant-isolated** (reuses tenancy — no new isolation code).
- `owner_prefix` (optional, default `false`) — when `true`, every object's `key` is stored under `{owner_id}/…` and the generated guard enforces that the first path segment equals the caller's owner id. This is the direct analog of Supabase's `storage.foldername(name)[1] = auth.uid()` folder-per-user pattern — **the dominant real-world Supabase storage shape**, made mechanical. Requires `owner`.
- `max_size` (optional) — per-object cap (`"5MB"`); over-limit ⇒ `413 JC0413` (existing).
- `allowed_mime` (optional) — content-type allowlist (`image/*` globs); violation ⇒ **new `JC0415`**.

Validation (`questions.rs`, gated behind `contract_version >= 2`):
- `owner` must reference a declared entity.
- `owner_prefix: true` requires `owner`.
- `public` + tenant-scoped owner allowed (public read, scoped write) but flagged in the design summary.
- A `storage` block requires `db` (auto-added like `jobs`); requires `auth` when any bucket is `private`.

## Object metadata (in `jerrycan-db`) — bytes in the store, truth in the DB

Mirrors Supabase precisely (`storage.objects` is a Postgres table there). Generated migration adds one table per app:

```
storage_objects(
  id         uuid pk,
  bucket     text not null,
  key        text not null,          -- path within bucket; {owner_id}/… when owner_prefix
  owner_id   <owner pk type> null,
  tenant_id  <tenant pk type> null,  -- present when bucket owner is tenant-scoped
  size       integer not null,
  mime       text not null,
  checksum   text not null,          -- sha256 hex; doubles as the ETag
  created_at datetime not null,
  unique(bucket, key)
)
```

The metadata row is the **source of truth for access**: listing, ownership, tenant isolation, and `owner_prefix` checks all run against this table using the exact repo + `Tenant`/owner guard machinery entities already use. The blob store holds only bytes, keyed by `bucket/key`. **Because listing is DB-backed, we never parse S3's `ListObjects` — object keys never round-trip through XML.**

## Generated surface (per bucket, guarded + tested)

For bucket `<b>`:

| Method + path | Behavior | Guard |
|---|---|---|
| `POST /<b>` | upload (v2.1 `Multipart`/`RawBody`); enforce `max_size` + `allowed_mime`; write bytes → store, row → db; `owner_id`/`tenant_id` from principal; prepend `{owner_id}/` when `owner_prefix` | auth + owner/Tenant |
| `GET /<b>` | list objects (owner/tenant/prefix-scoped) | scoped (private) / open (public) |
| `GET /<b>/{id}` | download bytes (or 302 → signed URL); emits `ETag` + `Cache-Control` | scoped (private) / open (public) |
| `DELETE /<b>/{id}` | delete row + bytes | auth + owner/Tenant |
| `POST /<b>/{id}/sign` | issue a time-limited signed URL | auth + owner/Tenant |

- Private-bucket mutations fall under the existing **`JL0004` unguarded-mutation lint** — the guard is generated on, not hand-rolled.
- `owner_prefix` adds a path-prefix assertion to every access check: a principal can't read/write/delete an object whose first key segment is another owner's id.
- Public `GET`s emit **`ETag`** (the sha256 checksum) + **`Cache-Control`** — cheap, expected, cache-friendly.
- **Generated isolation tests** (like entity tenancy): User-B gets `404` on User-A's object; cross-tenant blocked; cross-prefix blocked. **Negative-control-able** — breaking any scope turns the eval gate red.
- New JC code **`JC0415`** (unsupported media type) registered in `codes.rs` + documented in `docs/ai/13-error-codes.md`. Not-found is existing `404`; too-large is existing `JC0413`.

## Blob backends: Local (dev) + S3-compatible (prod)

One trait, two implementations behind a feature — matching the `store.rs` / `redis_store.rs` split in `jerrycan-ratelimit`.

```rust
// store.rs
pub trait BlobStore: Send + Sync {
    async fn put(&self, bucket: &str, key: &str, body: Bytes, mime: &str) -> Result<()>;
    async fn get(&self, bucket: &str, key: &str) -> Result<Bytes>;
    async fn delete(&self, bucket: &str, key: &str) -> Result<()>;
    async fn presign_get(&self, bucket: &str, key: &str, ttl: Duration) -> Result<Option<String>>;
}
```

- **`LocalStore`** (default, zero-config): filesystem under a root dir. `presign_get` returns `None` ⇒ caller falls back to app-HMAC signed URL. Dev + single-node self-host.
- **`S3Store`** (feature `storage-s3`): **built on jerrycan's own outbound HTTP stack** — `hyper_util::client::legacy::Client` + `hyper-rustls`, identical to `jerrycan-auth::oauth`. **No reqwest, no `object_store`, no `aws-sdk-s3`.**
  - **SigV4** signing via `hmac` + `sha2` (already vendored by auth) — no new crypto crate.
  - **XML** (S3 error `Code`/`Message`, multipart `UploadId`/`ETag`): parsed with **`quick-xml`** (pure-safe-Rust, MIT, no `unsafe`). This is the **only new third-party crate in the whole workspace.** `ListObjects` is never parsed (listing is DB-backed), so keys never touch XML.
  - **Multipart upload** for large objects (streaming, > part-size threshold).
  - `presign_get` = **SigV4 query-signing** → direct-to-bucket URL (Supabase `createSignedUrl` parity).
  - Works with AWS S3, Cloudflare R2, MinIO, Supabase's own S3 endpoint.
- **Config by env, zero-touch:** `JERRYCAN_STORAGE=local:/var/data` or `s3://bucket?region=…&endpoint=…`. `jerrycan package` already emits the container/k8s; storage config is env + secrets.

## Signed URLs

- **App-HMAC (universal default):** jerrycan signs `/<b>/<id>?exp=…&sig=…` (HMAC over `bucket|key|exp`), verifies in-handler, streams bytes. Works on **every** backend (local included); keeps the in-app guard + access log. App is in the data path.
- **S3 native presign (opt-in):** for S3 buckets, `presign_get` returns a native presigned URL so the client hits the bucket directly (no app bandwidth). Same SigV4 signer as `S3Store`. Closest Supabase parity.
- `POST /<b>/{id}/sign` picks native presign when the backend supports it, else app-HMAC.

## Supabase migration mapping (input to the migrator spec)

| Supabase | jerrycan-storage |
|---|---|
| `storage.buckets` row (`public`, `file_size_limit`, `allowed_mime_types`) | a `storage.buckets[]` entry |
| RLS `owner = auth.uid()` (per-object owner) | `owner: User` |
| RLS `storage.foldername(name)[1] = auth.uid()` (folder-per-user) | `owner_prefix: true` + `owner` |
| RLS tenant/team policy | `owner` = tenant-scoped entity |
| operation-asymmetric policy (public read, owner write) | `visibility: public` + per-endpoint guard |
| metadata condition (size / mimetype) | `max_size` / `allowed_mime` |
| join/share-based policy (`album_shares`, collaborators) | **gap report** (agent → handler guard) |
| `storage.objects` rows | `storage_objects` metadata seed |
| object bytes (Supabase S3) | copied → target `BlobStore` |
| image transformations | **gap report** (not supported v1) |

Result: same buckets, same access shape, tested green.

## Eval gate

The checked-in **reference Supabase export** (used by the migrator eval) gains buckets. The eval battery migrates them and asserts: endpoints green, cross-owner read `404`, cross-tenant blocked, cross-prefix blocked (negative controls). Un-skippable in CI + pre-publish, exactly like the reference slice today.

## Testing strategy

- Unit: SigV4 signer against AWS-published test vectors; app-HMAC round-trip; `max_size`/`allowed_mime`/`owner_prefix` enforcement; `quick-xml` parsing of real S3 **and MinIO/R2** error + multipart bodies (provider variance).
- Integration: `LocalStore` full CRUD; `S3Store` against a **MinIO** container in CI behind `storage-s3`.
- Generated-app tests: per-bucket acceptance + isolation (owner + tenant + prefix), negative controls.
- Docs: every `docs/ai` storage example is a CI doc-test.

## Resolved decisions (review 2026-07-10)

1. **HTTP client:** our own — `hyper-util` + `hyper-rustls`, matching `jerrycan-auth::oauth`. reqwest / object_store / aws-sdk-s3 rejected (second stack / dep weight / convention).
2. **XML:** `quick-xml` (S3 error + multipart only; listing is DB-backed). The single new workspace crate.
3. **Signed URLs:** app-HMAC universal default + S3 native presign opt-in (same SigV4 signer).
4. **Contract:** `design.json` contract_version 2 (v0/v1 remain valid).
5. **Metadata:** rows in `jerrycan-db` (`storage_objects`), source of truth for access; `storage` implies `db`.
6. **Backends:** `LocalStore` default + `S3Store` behind `storage-s3`.
7. **Public buckets:** emit `ETag` (sha256) + `Cache-Control`.
8. **Path scoping:** first-class `owner_prefix` — folder-per-user migrates mechanically.

## Known gaps (migrator reports these; not modeled in v1)

- **Join/share-based policies** (per-object share lists, collaborators) → gap report; agent writes a handler guard (jerrycan enforces cross-entity integrity in handlers).
- **Image transformations** (Supabase render API) → gap report.
- **Arbitrary JWT-claim conditions** beyond role gating → gap report.
