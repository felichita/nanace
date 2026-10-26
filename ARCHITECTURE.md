# nanace Architecture

nanace is a multi-tenant knowledge base: teams write markdown "contexts",
access is controlled by team-level RBAC, and Claude searches the contexts
semantically through MCP.

Status legend: **[built]** exists in this repository; **[planned]** is the
agreed design that is not implemented yet.

## Three Layers

### 1. Frontend (React) [planned]

User interface:

- Markdown editor for creating contexts
- Team management
- Permission controls
- Search and browsing of contexts

Talks to the backend over REST (and WebSocket for live updates).

### 2. Backend (Rust + Actix-web)

Handles requests, validation, and database integration:

- Authentication: Google OAuth 2.0 **[planned]** + JWT **[built]**. Until
  OAuth lands, `POST /api/v1/auth/dev-login` issues tokens for any email when
  `NANACE_DEV_LOGIN=true` (development only).
- Authorization: role-based access control (RBAC) **[built]**
- Embedding generation for markdown **[built]**
- API handlers: CRUD over contexts **[built]**
- MCP server exposing tools to Claude **[planned]**
- Audit logging of every action **[built]**

Talks to PostgreSQL and Qdrant.

### 3. Databases

**PostgreSQL (relational, source of truth)** [built]

| Table | Purpose |
|---|---|
| `organizations` | Companies (tenants) |
| `users` | People; one row per normalized email |
| `org_members` | Org membership with `member` / `admin` role |
| `teams` | dev, QA, devops, analytics, ... within an org |
| `team_members` | Team role: `reader` / `writer` / `admin`; `source` = `manual` or `google_sync` |
| `contexts` | Title, full markdown, tags, author, embedding bookkeeping |
| `audit_logs` | Who changed or read what, and when |

Roles are a closed set enforced by `CHECK` constraints and the
`nanace_core::TeamRole` / `OrgRole` enums, not a separate table.

**Qdrant (vector database)** [built]

- One collection per organization: `contexts_<org uuid>`.
- One point per markdown chunk:
  - vector: embedding of `"<title>\n\n<chunk>"`
  - payload: `org_id`, `team_id`, `context_id`, `chunk_index`, `text`
  - payload indexes on `team_id`, `context_id`, `chunk_index`
- Used for semantic search only. PostgreSQL holds the canonical content and
  permissions.

## Typical Flows

### Flow 1: Creating a context (developers write API docs)

```text
1. A developer writes markdown in the frontend:
   "## API Authentication"
   "POST /auth/login"
   "Returns JWT token"

2. The frontend sends it to the backend.

3. The backend:
   a) Checks that the developer may write to the team (PostgreSQL)
   b) Chunks the markdown and generates embeddings (local bge-small-en-v1.5)
   c) Inside one transaction scoped by RLS:
      - inserts the context into PostgreSQL
      - writes the audit entry (user=alice, action=context.create, resource=ctx_123)
   d) Upserts the chunk vectors into Qdrant, then commits

4. Done. The context is visible to its team (developers).
```

Vectors are written before the commit, so a Qdrant failure leaves nothing
committed. A commit failure after the upsert leaves orphan points, which are
harmless because every search result is re-checked in PostgreSQL.

### Flow 2: Claude looks something up

```text
1. A user asks Claude: "How do I authenticate users?"

2. Claude, via MCP [planned; the HTTP search endpoint is built]:
   a) The backend embeds the question
   b) Looks up the caller's readable teams in PostgreSQL
   c) Searches the org's Qdrant collection filtered to those teams
   d) Re-checks every hit against PostgreSQL under RLS
   e) Returns the markdown, e.g. "## API Authentication..."

3. Claude answers grounded in your documentation.
```

### Flow 3: Team isolation (QA cannot see DevOps)

```text
QA tries to read DevOps contexts:

1. The backend receives: user=bob (QA), org=example

2. Application check (PostgreSQL): bob has no role in team devops
   -> 404 for reads, 403 for writes

3. Database check (RLS): even a query without a team filter, run in bob's
   scoped transaction, returns only rows of teams bob belongs to.

4. Result: QA sees only QA contexts. DevOps contexts stay hidden.
```

Org admins manage teams and memberships but do not get implicit access to
team content; they must add themselves to a team to read it.

## Security and Multi-Tenancy

### Row-Level Security (PostgreSQL) [built]

Handlers that touch `contexts` run inside `db::scoped_tx`, which sets
`nanace.org_id` / `nanace.user_id` for the transaction and switches to the
`nanace_tenant` role. Policies (see `crates/nanace-server/migrations/`):

- `SELECT`: `org_id` matches the scope and the user is a member of the row's team.
- `INSERT` / `UPDATE` / `DELETE`: same, and the user's team role is `writer` or `admin`.

`contexts` uses `FORCE ROW LEVEL SECURITY` and has policies only for
`nanace_tenant`, so any code path that skips `scoped_tx` sees no rows.

RLS does not apply to superusers or `BYPASSRLS` roles. The application must
connect as an ordinary role (`deploy/postgres/init.sql` creates `nanace`); the
server logs a warning at startup otherwise, and the integration tests refuse
to run.

### Multi-tenant collections (Qdrant) [built]

```text
contexts_<example uuid>      <- every team of Example Corp, filtered by team_id
contexts_<startups uuid>  <- every team of Startups Inc
```

Different organizations use different collections, so their vectors never
mix. Inside an org, every query filters on `org_id` and on the caller's team
ids from PostgreSQL. This follows Qdrant's recommended payload-based
multitenancy and avoids a collection per team.

### Google Workspace integration [planned]

```text
Automatic sync:
dev-team@example.com -> Google Group -> sync -> team example/dev
qa-team@example.com  -> Google Group -> sync -> team example/qa
```

- Users are added to Google Groups.
- A sync runs every hour (`teams.google_group_email`,
  `team_members.source = 'google_sync'`).
- Permissions update automatically. Tokens carry only identity, and
  permissions are read on every request, so a revoked membership applies
  immediately.

## Embeddings [built]

- Model: `BAAI/bge-small-en-v1.5` (384 dimensions, English) running locally
  through `fastembed` / ONNX Runtime. No API key is needed; the model (~130 MB)
  is downloaded to `FASTEMBED_CACHE_DIR` on first start.
- Queries use bge's retrieval instruction prefix; passages do not.
- Markdown is split into chunks of at most 1500 bytes (`CHUNK_MAX_BYTES`), at
  headings first, then paragraphs, under the model's 512-token window.
- `contexts.embedding_hash` covers model, title, and content: edits that only
  change tags skip re-embedding.
- `HashingEmbedder` (`NANACE_EMBEDDER=hashing`) is a deterministic lexical
  stand-in for tests and offline work. It is not semantic.
- Changing the model requires re-indexing, because vectors from different
  models are not comparable.

## Scalability

**PostgreSQL** (metadata and permissions): can hold millions of contexts. The
bottleneck is joins on large datasets; the fix is indexing on
`(org_id, team_id, ...)` and `user_id`.

**Qdrant** (embeddings and search): can hold millions of vectors. The
bottleneck is memory; the fix is spreading collections across nodes.

**Rust backend** (throughput): 1000+ requests per second for metadata
operations. The bottleneck is embedding generation (CPU-bound, runs on the
blocking thread pool); the fix is batching, a dedicated embedding worker, and
caching.

## Component Diagram

```text
Frontend (React)
    | REST
Backend (Rust)
    |-> PostgreSQL  <- authorization, metadata, canonical content
    |-> Qdrant      <- embeddings, semantic search
    |
    | MCP protocol
Claude Desktop / Claude Code
    | question
MCP server (in the backend)
    |-> embed the question
    |-> search Qdrant + filter by PostgreSQL permissions

Result: accurate answers from your knowledge base.
```

## Why This Architecture

**PostgreSQL + Qdrant (not SurrealDB alone)**

- PostgreSQL: proven and mature, ideal for relational data and RBAC.
- Qdrant: optimized for vectors, semantic search, and RAG.
- Separation of concerns: each database does what it is best at.

**Rust backend (not Node.js)**

- Memory safety and no data races.
- Faster for CPU-heavy work such as embedding and crypto.
- Single binary with no GC pauses. (Bundling ONNX Runtime for local embeddings
  makes it larger than a minimal Rust binary.)
- `secrecy` wraps secrets so they are redacted from logs and zeroized on drop.

**Collection per organization (not one shared collection)**

- No cross-org leakage at the index level.
- Easy to audit and to delete one tenant.
- Each organization can be scaled separately.

## Repository Layout

| Path | Contents |
|---|---|
| `crates/nanace-core` | Pure domain logic: roles, validation, markdown chunking, hashing |
| `crates/nanace-embed` | `Embedder` trait, `FastEmbedder` (bge-small), `HashingEmbedder` |
| `crates/nanace-server` | Actix-web API, auth, RBAC, RLS-scoped DB access, Qdrant, audit, migrations |
| `deploy/postgres` | Local database bootstrap (non-superuser app role) |
| `docker-compose.yml` | PostgreSQL 18 and Qdrant for development |

## API (v1)

| Method | Path | Who |
|---|---|---|
| `POST` | `/api/v1/auth/dev-login` | Anyone, only when dev login is enabled |
| `GET` | `/api/v1/me` | Authenticated |
| `POST` | `/api/v1/orgs` | Authenticated; caller becomes org admin |
| `GET/POST` | `/api/v1/orgs/{org}/teams` | Org member / org admin |
| `GET/PUT` | `/api/v1/orgs/{org}/teams/{team}/members` | Team member or org admin / team admin or org admin |
| `DELETE` | `/api/v1/orgs/{org}/teams/{team}/members/{user}` | Team admin or org admin |
| `GET/POST` | `/api/v1/orgs/{org}/teams/{team}/contexts` | Team reader / team writer |
| `GET/PUT/DELETE` | `/api/v1/orgs/{org}/contexts/{id}` | Team reader / team writer / team writer |
| `POST` | `/api/v1/orgs/{org}/search` | Org member; results limited to readable teams |
| `GET` | `/api/v1/orgs/{org}/audit-logs` | Org admin |
| `GET` | `/healthz` | Anyone |

Resources the caller may not know about return `404`, so ids cannot be probed
across teams or orgs.

## Summary

**In one sentence:** PostgreSQL + Qdrant give a multi-tenant knowledge base
with team-level RBAC, Google Groups auto-sync, and Claude semantic search.

**For managers:**

- Teams see only their own data.
- Permissions sync automatically from Google Groups.
- Claude can search the knowledge base.
- Every action is logged for compliance.

**For engineers:**

- PostgreSQL RLS for row-level security
- Qdrant multi-tenant collections
- MCP for the Claude integration
- Rust for performance and safety
