# nanace Architecture

nanace is a multi-tenant knowledge base: teams write markdown "contexts",
access is controlled by team-level RBAC, and Claude searches the contexts
semantically through MCP.

Status legend: **[built]** exists in this repository; **[planned]** is the
agreed design that is not implemented yet.

## Three Layers

### 1. Desktop app (Tauri + React) [built]

`apps/desktop`: a desktop app with a page tree, a block editor and a graph view.

- **Shell:** Tauri 2 (Rust). The webview never holds credentials: it calls
  one `api` command, which attaches a personal access token kept in the OS
  keychain and only reaches `<server>/api/v1/...`. External links open in the
  system browser; the webview cannot navigate away from the app.
- **UI:** React, TypeScript, Vite, Tailwind, TanStack Query.
  - Sidebar with workspaces (orgs), teamspaces and pages.
  - BlockNote block editor with markdown round-trip. Readers get a read-only
    view.
  - Page mentions: type `@` to link a page. Mentions are stored as
    `[[Title]]`.
  - ⌘K search: title matches plus semantic results.
  - Backlinks, links and related pages under each page.
  - Force-directed graph view (react-force-graph).
- **Editing:**
  - Autosave sends `base_updated_at`. When the page changed in the meantime,
    the server answers `409`, and the app offers a reload instead of
    overwriting.
- **Claude memory import:**
  - **Reading:** the button reads Claude Code memory files
    (`~/.claude/projects/*/memory/*.md`) through one read-only Tauri command.
  - **Conversion:** each memory becomes a page. Its title comes from the
    project's `MEMORY.md`, `[[name]]` links are rewritten to `[[Title]]`,
    and the page gets the tags `claude-memory`, the project and the memory
    type.
  - **Re-import:** a `Source:` footer identifies the file, so importing again
    skips it, or updates it only when the user asks.
- **Connect to Claude:**
  - **Bundling:** `nanace-mcp` ships as a Tauri sidecar.
  - **Registration:** the button registers it with Claude Code
    (`claude mcp add --scope user`) and/or Claude Desktop (merges the
    `nanace` entry into `claude_desktop_config.json`).
  - **Tokens:** each app gets its own 30-day token, read-scoped or, when
    "Allow Claude to create and edit pages" is checked, write-scoped with
    `NANACE_ALLOW_WRITES=true`.
    - **Issuing:** needs a fresh interactive sign-in, and revokes every
      earlier token of that kind.
    - **Rotation:** while the app runs it rotates tokens older than 7 days
      (`POST /me/tokens/current/rotate`) and rewrites the configs. It notifies
      the user when a token can no longer be renewed. Reconnecting revokes the previous token, and
    disconnecting revokes it with itself.
- **Sign-in:** Google in the system browser with a loopback redirect and PKCE
  (RFC 8252), or dev email login. Both end with a write-scoped personal
  access token. Signing out revokes that token.

Live collaboration (WebSocket, CRDT) is **[planned]**.

### 2. Backend (Rust + Actix-web)

Handles requests, validation, and database integration:

- Authentication **[built]**: Google sign-in (OpenID Connect) issuing
  short-lived session JWTs, plus personal access tokens for MCP and scripts.
  `POST /api/v1/auth/dev-login` issues sessions for any email when
  `NANACE_DEV_LOGIN=true` (development only).
- Authorization: role-based access control (RBAC) **[built]**
- Embedding generation for markdown **[built]**
- API handlers: CRUD over contexts **[built]**
- MCP server exposing tools to Claude **[built: stdio; read, plus opt-in create/update]**
- Audit logging of every action **[built]**

Talks to PostgreSQL and Qdrant.

### 3. Databases

**PostgreSQL (relational, source of truth)** [built]

| Table | Purpose |
|---|---|
| `organizations` | Companies (tenants) |
| `users` | People; one row per normalized email, linked to a Google subject |
| `api_tokens` | Personal access tokens (hash only), scope, expiry, revocation |
| `login_codes` | One-time codes handed to the browser after Google sign-in |
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

2. Claude calls the `search_contexts` MCP tool; `nanace-mcp` forwards it to
   the API as the user, and the backend:
   a) Embeds the question
   b) Looks up the caller's readable teams in PostgreSQL
   c) Searches the org's Qdrant collection filtered to those teams
   d) Re-checks every hit against PostgreSQL under RLS
   e) Returns the markdown, e.g. "## API Authentication..."

3. Claude reads the full document with `get_context` and answers grounded
   in your documentation.
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

### Authentication [built]

**Google sign-in** (authorization code flow with PKCE):

```text
Browser -> GET /api/v1/auth/google/login
        <- 302 to Google + signed state cookie (state, nonce, PKCE verifier; 10 min)
Google  -> GET /api/v1/auth/google/callback?code&state
           backend checks cookie signature + state, exchanges the code with
           the client secret + verifier, validates ID token claims
           (iss, aud, exp, nonce, email_verified, Workspace domain)
        <- 302 to NANACE_FRONTEND_URL?code=<one-time login code>  (or ?error=<reason>)
Frontend -> POST /api/v1/auth/exchange {code}  -> session JWT (single use, 60 s)
```

The desktop app adds two parameters to the login request:
`redirect_port` (1024 or above) and an S256 `code_challenge`.

- Both are signed into the state cookie.
- The callback then redirects to `http://127.0.0.1:<port>/callback`.
- The issued login code can only be exchanged with the matching
  `code_verifier`.
- A wrong or missing verifier still consumes the code.

- The ID token is not signature-checked: it comes straight from Google's
  token endpoint over TLS, which OpenID Connect Core §3.1.3.7 allows.
- Users are matched by Google subject, then by email for people invited
  before their first sign-in. An email already linked to a different Google
  account is rejected (`account_conflict`), never taken over.
- `NANACE_GOOGLE_ALLOWED_DOMAINS` restricts sign-in to Workspace accounts of
  those domains (the `hd` claim), not merely matching email suffixes.
- Session token in the URL is avoided: only the one-time code is, and the
  landing page strips it and sends `Referrer-Policy: no-referrer`.
- Browsers land on a built-in page (`/auth/complete`) by default. It lets
  you sign in and manage access tokens.

**Personal access tokens** (`nnc_pat_…`):

- 256-bit random secrets; only the SHA-256 hash and a short display prefix
  are stored. Shown once at creation.
- Scope `read` (GET requests and search) or `write` (everything the user's
  roles allow). Mutating handlers take `WriteUser`, which rejects read tokens.
- Expire after 1–365 days (default 90); revocation is immediate. At most 50
  active tokens per user.
- Creating and revoking tokens by id requires an interactive session, so a
  leaked token cannot mint unrelated ones.
- **Rotation:** a token can replace itself (`/me/tokens/current/rotate`) with
  the same name, scope and lifetime.
  - The old token keeps working for a 24-hour grace period but cannot rotate
    again, so a chain never forks.
  - A chain ends one year after its first token was issued, so a leaked token
    cannot keep itself alive forever.
  - Revoking any token of a chain revokes the whole chain (`chain_id`), so a
    copy that a thief rotated dies with the original. The token list shows
    `rotated_at`.
- Tokens carry the user's identity only; team permissions still come from
  PostgreSQL on every request.

### Links and graph [built]

- **Link syntax:** a context links to another with `[[Title]]` or
  `[[Title|label]]`. Text in code is ignored.
- **Storage:** on every create or update, the links are stored in
  `context_links` as `lower(title)` keys.
- **Resolution:** keys are resolved at read time by joining `contexts` on
  `lower(title)`. Both tables are under RLS, so links only resolve to
  contexts the reader can see, and a link starts resolving once its target
  page is created.
- **Similarity edges:** Qdrant nearest-neighbour queries from each context's
  first-chunk vector, filtered by org and readable teams. Only edges between
  nodes read from PostgreSQL are returned.
- **Not audited:** the graph and links endpoints return titles only, like
  listing, so they are not audited as content reads.

### Trash [built]

- **Below the API** (migration 0009): a trash row must mirror a live context
  (same id, org, team, author and creation time), and a trigger keeps a
  context's id, org, team, author and creation time from ever changing.

- **Deleting:** deleting a context moves it to `context_trash` and removes its
  vectors and links. Trashed pages are outside `contexts`, so search, the
  graph and MCP never see them.
- **Who has access:** RLS limits the trash to team writers and admins.
- **Restoring:** a restore puts the row back with its id and original author,
  and re-embeds the content. The `contexts` INSERT policy accepts another
  user's `created_by` only for a row coming back from the trash.
- **Purging** deletes the trashed row permanently.
- **Retention:** trashed contexts are purged for good after
  `NANACE_TRASH_RETENTION_DAYS` days (default 7; 0 keeps them). An hourly
  server task does this under RLS as a writer of each team and audits it as
  `system.trash_purge`. The trash list shows each page's `purge_at`.

### Google Workspace group sync [planned]

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

## Upgrades [built]

`src/upgrades.rs` upgrades an installation in two layers, under a PostgreSQL
advisory lock so replicas starting together upgrade once.

1. **SQL migrations** (`migrations/`, embedded in the binary, tracked in
   `_sqlx_migrations`) change the schema.
2. **Application upgrades** are numbered steps written in Rust for what SQL
   cannot do, tracked in `app_upgrades`, like a module version map.
   - **Upgrade 1** backfills wiki links of contexts created before
     `context_links` existed, writing as a team writer under RLS.
   - **Model changes:** `system_state.embedding_model` records which model
     produced the vectors. A different configured model triggers a full
     reindex.

The server runs both at startup unless `NANACE_RUN_MIGRATIONS=false`;
`nanace-server migrate` runs them as a separate step. With migrations off, the
server compares the database with its own lists (`migrate --check` does the
same):

- it refuses to start when anything is pending;
- it refuses to start when the database has migrations or upgrades it does
  not know, which means a newer version upgraded it.

## Embeddings [built]

`nanace-server reindex [--org <id>]` rebuilds Qdrant from PostgreSQL, for
example after moving to a new Qdrant or changing the model:

- **What it does:** it recreates each org's collection with the current
  model's dimensions and re-embeds every live (not trashed) context with the
  same chunking as the API.
- **RLS:** contexts are read through `db::scoped_tx` as a member of their
  team, so RLS still applies. Teams without members cannot be read and are
  reported.
- **During the run:** search for that org is incomplete until it finishes.

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

## MCP Server [built]

`nanace-mcp` is a separate binary that Claude Desktop or Claude Code launches
over stdio. It calls the REST API with the user's credentials and holds no
database or Qdrant access, so every tool result passes the same RBAC, RLS and
audit path as the HTTP API.

| Tool | API call | Purpose |
|---|---|---|
| `list_teams` | `GET /me` | Orgs, teams and roles, with ids |
| `search_contexts` | `POST /orgs/{org}/search` | Semantic search with snippets |
| `get_context` | `GET /orgs/{org}/contexts/{id}` | Full markdown of one context |
| `list_contexts` | `GET /orgs/{org}/teams/{team}/contexts` | Recent titles in a team |
| `create_context` | `POST /orgs/{org}/teams/{team}/contexts` | New page (opt-in) |
| `update_context` | `PUT /orgs/{org}/contexts/{id}` | Append to or replace a page (opt-in) |

- **Read tools** are annotated read-only.
- **Write tools** exist only when the server runs with
  `NANACE_ALLOW_WRITES=true`, and the API also requires a write-scoped token
  and a writer role in the team.
- **No delete tool.** The server instructions tell the model to write only
  when the user asks, never because a document says so.
- **Concurrent edits:** a full replacement must pass the `updated` value
  of the `get_context` it is based on as `base_updated_at`; appends use the
  version read just before writing. Either way a concurrent edit returns a
  conflict instead of being overwritten.
- **Delimiters:** page text and search snippets cannot close or fake the
  `<context_content>` delimiters.
- The org is the tool argument, then `NANACE_ORG_ID`, then the user's only org.
- Context content is returned inside `<context_content>` delimiters, and the
  server instructions tell the model to treat it as data, not instructions.
- Authentication: `NANACE_TOKEN` with a personal access token (a `read`
  token is enough), or `NANACE_DEV_EMAIL` against a development API. Remote
  MCP over streamable HTTP with OAuth is **[planned]**.

## Component Diagram

```text
Frontend (React)
    | REST
Backend (Rust)
    |-> PostgreSQL  <- authorization, metadata, canonical content
    |-> Qdrant      <- embeddings, semantic search
    ^
    | REST, as the user
nanace-mcp (stdio)
    ^
    | MCP protocol
Claude Desktop / Claude Code
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
| `crates/nanace-mcp` | stdio MCP server over the REST API; read tools, opt-in create/update |
| `apps/desktop` | Tauri desktop app (React UI in `src`, Rust shell in `src-tauri`) |
| `deploy/postgres` | Local database bootstrap (non-superuser app role) |
| `docker-compose.yml` | PostgreSQL 18 and Qdrant for development |

## API (v1)

| Method | Path | Who |
|---|---|---|
| `POST` | `/api/v1/auth/dev-login` | Anyone, only when dev login is enabled |
| `GET` | `/api/v1/auth/google/login`, `/callback` | Browser sign-in, when Google is configured |
| `POST` | `/api/v1/auth/exchange` | One-time login code holder |
| `GET/POST` | `/api/v1/me/tokens` | Authenticated / session only |
| `DELETE` | `/api/v1/me/tokens/{id}` | Session only |
| `DELETE` | `/api/v1/me/tokens/current` | The token itself (sign-out) |
| `POST` | `/api/v1/me/tokens/current/rotate` | The token itself; same name, scope and lifetime |
| `GET` | `/api/v1/me` | Authenticated |
| `POST` | `/api/v1/orgs` | Authenticated; caller becomes org admin |
| `GET` | `/api/v1/orgs/{org}/members` | Org admin; members with their teams |
| `PUT` | `/api/v1/orgs/{org}/members/{user}` | Org admin; role `admin` or `member` (not the last admin) |
| `DELETE` | `/api/v1/orgs/{org}/members/{user}` | Org admin; removes from the org and all its teams (not the last admin) |
| `GET/POST` | `/api/v1/orgs/{org}/teams` | Org member / org admin |
| `GET/PUT` | `/api/v1/orgs/{org}/teams/{team}/members` | Team member or org admin / team admin or org admin |
| `DELETE` | `/api/v1/orgs/{org}/teams/{team}/members/{user}` | Team admin or org admin |
| `GET/POST` | `/api/v1/orgs/{org}/teams/{team}/contexts` | Team reader / team writer |
| `GET/PUT/DELETE` | `/api/v1/orgs/{org}/contexts/{id}` | Team reader / team writer / team writer |
| `GET` | `/api/v1/orgs/{org}/contexts/{id}/links` | Team reader; outgoing, backlinks, related |
| `GET` | `/api/v1/orgs/{org}/graph` | Org member; readable contexts, link and similarity edges |
| `GET` | `/api/v1/orgs/{org}/trash` | Team writer; trashed pages of writable teams |
| `POST` | `/api/v1/orgs/{org}/trash/{id}/restore` | Team writer |
| `DELETE` | `/api/v1/orgs/{org}/trash/{id}` | Team writer; permanent |
| `POST` | `/api/v1/orgs/{org}/search` | Org member; results limited to readable teams |
| `GET` | `/api/v1/orgs/{org}/audit-logs` | Org admin |
| `GET` | `/livez` | Anyone; liveness, touches no dependencies |
| `GET` | `/readyz`, `/healthz` | Anyone; readiness, checks PostgreSQL and Qdrant |

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
