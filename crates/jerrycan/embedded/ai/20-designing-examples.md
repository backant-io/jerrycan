# Designing: worked examples

## Purpose
Complete, copy-ready `design.json` examples for the `jerrycan docs designing`
contract — lift one as a starting point and adapt it. Read `jerrycan docs designing`
first for the normative schema (every field, rule, and gotcha); this page is
illustration only. Every design below is parsed straight from the served page bytes
and run through the real validator and scaffolder by the test suite, so each is
guaranteed to validate and scaffold.

### Minimal app
The smallest valid design — one module, one public endpoint, no db:
```json
{
  "name": "hello-api",
  "contract_version": 1,
  "description": "The smallest valid design: one module, one endpoint.",
  "modules": [
    {
      "name": "health",
      "endpoints": [
        {
          "operation_id": "liveness",
          "method": "GET",
          "path": "/",
          "public": true,
          "success": { "status": 200 }
        }
      ]
    }
  ]
}
```

### CRUD module (fields + enum + unique + index)
```json
{
  "name": "catalog-api",
  "contract_version": 1,
  "description": "A CRUD module backed by SQL: fields, an enum, unique + index.",
  "dependencies": ["db"],
  "modules": [
    {
      "name": "products",
      "entities": [
        {
          "name": "Product",
          "fields": [
            { "name": "sku", "type": "string", "unique": true, "index": true },
            { "name": "name", "type": "string" },
            { "name": "price_cents", "type": "integer" },
            { "name": "status", "type": "string", "values": ["draft", "active", "archived"] },
            { "name": "in_stock", "type": "boolean", "required": false },
            { "name": "attributes", "type": "json", "required": false }
          ]
        }
      ],
      "endpoints": [
        {
          "operation_id": "list_products",
          "method": "GET",
          "path": "/",
          "success": { "status": 200, "entity": "Product", "list": true }
        },
        {
          "operation_id": "create_product",
          "method": "POST",
          "path": "/",
          "request_body": { "entity": "Product" },
          "success": { "status": 201, "entity": "Product" },
          "errors": [{ "status": 409, "code": "JC0409", "when": "sku already exists" }]
        },
        {
          "operation_id": "show_product",
          "method": "GET",
          "path": "/{id}",
          "success": { "status": 200, "entity": "Product" },
          "errors": [{ "status": 404, "code": "JC0404", "when": "unknown id" }]
        },
        {
          "operation_id": "delete_product",
          "method": "DELETE",
          "path": "/{id}",
          "success": { "status": 204 },
          "errors": [{ "status": 404, "code": "JC0404", "when": "unknown id" }]
        }
      ]
    }
  ]
}
```

### Relations (`belongs_to` + `on_delete`)
A `Comment` belongs to a `Post` in the SAME module, so the fk is a real
DB-enforced `FOREIGN KEY` with `ON DELETE CASCADE`:
```json
{
  "name": "blog-api",
  "contract_version": 1,
  "description": "Relations: a Comment belongs_to a Post (same module, cascade).",
  "dependencies": ["db"],
  "modules": [
    {
      "name": "posts",
      "entities": [
        {
          "name": "Post",
          "fields": [
            { "name": "title", "type": "string" },
            { "name": "body", "type": "string" }
          ]
        },
        {
          "name": "Comment",
          "belongs_to": [{ "entity": "Post", "on_delete": "cascade" }],
          "fields": [
            { "name": "author", "type": "string" },
            { "name": "text", "type": "string" }
          ]
        }
      ],
      "endpoints": [
        {
          "operation_id": "list_posts",
          "method": "GET",
          "path": "/",
          "success": { "status": 200, "entity": "Post", "list": true }
        },
        {
          "operation_id": "create_post",
          "method": "POST",
          "path": "/",
          "request_body": { "entity": "Post" },
          "success": { "status": 201, "entity": "Post" }
        },
        {
          "operation_id": "create_comment",
          "method": "POST",
          "path": "/{id}/comments",
          "request_body": { "entity": "Comment" },
          "success": { "status": 201, "entity": "Comment" },
          "errors": [{ "status": 404, "code": "JC0404", "when": "unknown post id" }]
        }
      ]
    }
  ]
}
```

### First-class tenancy
`tenancy.entity` names the tenant; any entity that `belongs_to` it becomes
tenant-owned. The generator emits a `<workspace>_members` membership table,
tenant-SCOPED repositories (`all_for`/`get_for`/…), a Tenant guard, and
cross-tenant ISOLATION tests. Tenancy needs an active auth model
(`session`/`jwt`):
```json
{
  "name": "crm-api",
  "contract_version": 1,
  "description": "First-class tenancy: Leads belong to a Workspace tenant.",
  "auth": { "model": "session", "roles": ["owner", "member"] },
  "dependencies": ["db", "auth"],
  "tenancy": { "entity": "Workspace", "member_roles": ["owner", "member"] },
  "modules": [
    {
      "name": "workspaces",
      "entities": [
        {
          "name": "Workspace",
          "fields": [{ "name": "name", "type": "string" }]
        }
      ],
      "endpoints": [
        {
          "operation_id": "create_workspace",
          "method": "POST",
          "path": "/",
          "auth_required": true,
          "request_body": { "entity": "Workspace" },
          "success": { "status": 201, "entity": "Workspace" }
        }
      ]
    },
    {
      "name": "leads",
      "entities": [
        {
          "name": "Lead",
          "belongs_to": [{ "entity": "Workspace", "on_delete": "cascade" }],
          "fields": [
            { "name": "email", "type": "string", "unique": true },
            { "name": "stage", "type": "string", "values": ["new", "qualified", "won", "lost"] }
          ]
        }
      ],
      "endpoints": [
        {
          "operation_id": "list_leads",
          "method": "GET",
          "path": "/",
          "auth_required": true,
          "success": { "status": 200, "entity": "Lead", "list": true }
        },
        {
          "operation_id": "create_lead",
          "method": "POST",
          "path": "/",
          "required_roles": ["owner"],
          "request_body": { "entity": "Lead" },
          "success": { "status": 201, "entity": "Lead" }
        }
      ]
    }
  ]
}
```

### Auth + a public route
A `jwt` model with roles; a guarded `/me`, a role-gated delete, and a `public`
signup (the only unauthenticated route). `password_hash` carries
`write_only: true`, so it is accepted on signup but never appears in a response
— a `password_hash` column is auto-hidden even without the flag
(secure-by-default):
```json
{
  "name": "accounts-api",
  "contract_version": 1,
  "description": "Auth with roles, a guarded route, and a public signup route.",
  "auth": { "model": "jwt", "roles": ["admin", "user"] },
  "dependencies": ["db", "auth"],
  "modules": [
    {
      "name": "accounts",
      "entities": [
        {
          "name": "Account",
          "fields": [
            { "name": "email", "type": "string", "unique": true },
            { "name": "password_hash", "type": "string", "write_only": true },
            { "name": "role", "type": "string", "values": ["admin", "user"] }
          ]
        }
      ],
      "endpoints": [
        {
          "operation_id": "signup",
          "method": "POST",
          "path": "/signup",
          "public": true,
          "request_body": { "entity": "Account" },
          "success": { "status": 201, "entity": "Account" }
        },
        {
          "operation_id": "me",
          "method": "GET",
          "path": "/me",
          "auth_required": true,
          "success": { "status": 200, "entity": "Account" }
        },
        {
          "operation_id": "delete_account",
          "method": "DELETE",
          "path": "/{id}",
          "required_roles": ["admin"],
          "success": { "status": 204 },
          "errors": [{ "status": 404, "code": "JC0404", "when": "unknown id" }]
        }
      ]
    }
  ]
}
```

### Jobs / cron
Declared at the TOP LEVEL; jobs require the `db` dependency. `schedule` (5-field
cron) makes a job a cron job; a job with no `schedule` is enqueued
programmatically:
```json
{
  "name": "billing-api",
  "contract_version": 1,
  "description": "Background jobs: an hourly cron plus a programmatic queue job.",
  "dependencies": ["db"],
  "jobs": [
    { "name": "expire_trials", "schedule": "0 * * * *", "queue": "billing" },
    { "name": "send_receipt", "queue": "email" }
  ],
  "modules": [
    {
      "name": "invoices",
      "entities": [
        {
          "name": "Invoice",
          "fields": [
            { "name": "amount_cents", "type": "integer" },
            { "name": "paid", "type": "boolean", "required": false }
          ]
        }
      ],
      "endpoints": [
        {
          "operation_id": "list_invoices",
          "method": "GET",
          "path": "/",
          "success": { "status": 200, "entity": "Invoice", "list": true }
        }
      ]
    }
  ]
}
```

### Signed webhook endpoint
A webhook is an unauthenticated POST whose only proof is an HMAC signature.
Model it as a `public` endpoint with a `signature`-mentioning error case (the
auth lint recognizes signature auth and won't flag it); verify the signature
IN-HANDLER over `RawBody` (see Auth):
```json
{
  "name": "payments-api",
  "contract_version": 1,
  "description": "A signed webhook: a public POST whose proof is an HMAC signature.",
  "modules": [
    {
      "name": "webhooks",
      "endpoints": [
        {
          "operation_id": "stripe_webhook",
          "method": "POST",
          "path": "/stripe",
          "public": true,
          "success": { "status": 204 },
          "errors": [
            { "status": 401, "code": "JC0401", "when": "missing or invalid HMAC signature" }
          ]
        }
      ]
    }
  ]
}
```
