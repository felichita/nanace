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
    - **Storage:** the token never appears in a config file or on a command
      line. It lives in a file only the user can read
      (`<app config>/claude/*.token`, mode 0600), and the configs carry
      `NANACE_TOKEN_FILE`, which `nanace-mcp` re-reads on every request.
    - **Rotation:** while the app runs it rotates tokens older than 7 days
      (`POST /me/tokens/current/rotate`) and rewrites the configs. It notifies
      the user when a token can no longer be renewed. Reconnecting revokes the previous token, and
    disconnecting revokes it with itself.
- **Sign-in:** Google in the system browser with a loopback redirect and PKCE
  (RFC 8252), or dev email login. Both end with a write-scoped personal
  access token. Signing out revokes that token.

- **Agents (P2P):**
  - **Sidecar:** `nanace-p2p` ships next to `nanace-mcp`. With P2P on, the
    app starts it, hands it the peer key (from the OS keychain) and a
    one-time supervisor secret on standard input, and keeps that pipe open:
    the daemon stops when the app does.
  - **Token:** turning P2P on needs a fresh interactive sign-in and issues a
    30-day token named `nanace p2p` with the `p2p` scope, stored in a file
    only the user can read and rotated weekly like Claude's. Turning it off
    revokes the token and forgets the peer key.
  - **Settings:** per team, whether this computer takes part and how
    requests are accepted (never more than the team's policy allows), whether
    answers are shown before sending, the trusted list, and "Stop
    everything". A teamspace owner can create the team's `Policy: P2P` page
    from the template here.
  - **Consent window:** who asks, in which team, the text, the deadline;
    allow, deny, or allow that person for some hours. Text from a peer is
    shown as plain text.
  - **Activity:** connections (direct or through the relay) and recent
    exchanges, without their text.
  - **Claude:** when P2P is on, "Connect to Claude" also gives Claude the
    `p2p_*` tools (`NANACE_ALLOW_P2P`, the socket and the secret file path).
  - The webview reaches the daemon through one command with a fixed list of
    methods. Sending and answering as an agent are not on it.

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
| `team_members` | Team role: `reader` / `agent` / `writer` / `admin`; `source` = `manual` or `google_sync` |
| `contexts` | Title, full markdown, tags, author, embedding bookkeeping |
| `audit_logs` | Who changed or read what, and when |
| `p2p_peers` | Which libp2p peer id belongs to which user; revoked ids stay as tombstones |
| `p2p_presence` | Which peers and agents are present in a team right now; rows expire |
| `p2p_policies` | The parsed `Policy: P2P` page of a team; one row per team |
| `p2p_exchanges` | Journal of P2P exchanges: signed metadata, never message text |

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
- Role `agent`: may `INSERT` its own rows and `UPDATE` rows it created; it
  has no `DELETE` and no access to the trash.
- Protected titles: when the row's title is protected (below), the role must
  be `admin`.

`contexts` uses `FORCE ROW LEVEL SECURITY` and has policies only for
`nanace_tenant`, so any code path that skips `scoped_tx` sees no rows.

RLS does not apply to superusers or `BYPASSRLS` roles. The application must
connect as an ordinary role (`deploy/postgres/init.sql` creates `nanace`); the
server logs a warning at startup otherwise, and the integration tests refuse
to run.

### Protected pages [built]

A page whose title starts with `Wiki:` or `Policy:` holds a team's rules: the
wiki schema and index, and team policies. Only an admin of that team (the
teamspace owner) may create, edit, rename, trash, restore or purge it.
Everyone else in the team reads it.

| Team role | Shown in the app as | Ordinary pages | `Policy:` / `Wiki:` pages |
|---|---|---|---|
| `admin` | Owner | Write, trash, restore, purge | Write |
| `writer` | Editor | Write, trash, restore, purge | Read |
| `agent` | Agent | Create; edit only pages it created; no trash | Read |
| `reader` | Reader | Read | Read |

**Role `agent`** is for service accounts that only agents use (ordinary
users with their own email). An edit to a page someone else created is
refused with `403` and the code `not_author`, and audited as
`context.agent_denied`. People with write access edit an agent's pages as any
other. "Pages it created" is compared by user: agents that share one account
can edit each other's pages, and the audit log shows that one account for all
of them.

- **Org admins** get no exception. An org admin who needs to edit such a page
  makes themselves a team admin first, which is audited as `team.member.put`.
- **Recognizing the title.** `nanace_core::validate::is_protected_title` and
  SQL `nanace_is_protected_title` apply the same steps and are tested with
  the same cases:
  1. Unicode NFKC, which folds full-width and compatibility forms.
  2. Invisible format characters (zero-width space, joiners, BOM, bidi
     marks) are dropped.
  3. Leading white space is skipped and only ASCII case is folded, so the
     answer does not depend on a locale.
  4. The title is protected when what remains starts with `wiki` or
     `policy`, optional white space and `:`.
- **Renames** are checked against the title before and after, so a page
  moves neither out of nor into a protected title.
- **Two layers.** Handlers answer `403` with the code `protected_page` and
  audit the refusal as `context.protected_denied`. The RLS policies on
  `contexts` and `context_trash` apply the same rule, so a handler that
  missed the check still could not write.
- **Look-alikes.** A title that only reads like a protected one (`Wіki:`
  with a Cyrillic letter) is a different title: nothing that looks pages up
  by title finds it. Handlers still refuse it from non-admins, so that a
  writer cannot plant a page a person would mistake for the team's policy.
  This check is in the handlers only.
- **Existing pages.** Nothing is migrated. `Wiki:` pages that writers
  created earlier stay as they are; from this version writers no longer edit
  them.
- **Clients.** The desktop app opens such a page read-only for writers and
  does not send a rename into a protected title. `nanace-mcp` returns a tool
  error saying who can edit. The `/nanace` skill creates and updates `Wiki:`
  pages only for the teamspace owner and otherwise prints the index lines for
  the owner.

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
- Scope `p2p` is for the P2P daemon. It reaches the peer registry
  (`/me/peers*`, presence, team peers, the team policy, exchange reports),
  `GET /me`, and rotating or revoking itself, and nothing else: `AuthUser` rejects it, so every other handler
  answers `403`. A token file taken from a laptop cannot read or change pages.
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
  Writers see a trashed `Policy:` or `Wiki:` page in the list but only a team
  admin restores or purges it.
- **Restoring:** a restore puts the row back with its id and original author,
  and re-embeds the content. The `contexts` INSERT policy accepts another
  user's `created_by` only for a row coming back from the trash.
- **Purging** deletes the trashed row permanently.
- **Retention:** trashed contexts are purged for good after
  `NANACE_TRASH_RETENTION_DAYS` days (default 7; 0 keeps them). An hourly
  server task does this under RLS as an admin of each team, or as a writer
  when the team has no admin, and audits it as `system.trash_purge`. A team
  without an admin keeps its trashed `Policy:` and `Wiki:` pages until it has
  one; the rest of its trash is still purged. The trash list shows each
  page's `purge_at`.

### Account and organization deletion [built]

**`DELETE /me`** (session only) erases the caller's account:

- **What goes:** memberships, tokens and login codes are deleted.
- **What stays:** the user row is kept because pages and audit entries
  reference it, but it is anonymized and marked `deleted_at`. Sessions and
  tokens stop working on the next request.
- **Organizations:** those where the caller is the only member are deleted.
  Pages in shared teams stay with the team.
- **Last admin:** refused while the caller is the last admin of an
  organization that still has members.
- **Signing in again** with the same email starts a new, empty account.

**`DELETE /orgs/{org}`** (org admin, confirmed with the slug) deletes the
organization:

- **What goes:** teams, pages, links, trash, the org's audit history and its
  Qdrant collection. Pages are removed by the foreign-key cascade, which
  PostgreSQL runs without row security.
- **What stays:** an `org.delete` entry with no org.

### P2P registry [built]

Agents on different machines talk to each other over libp2p (`nanace-p2p`,
[below](#agent-p2p-nanace-p2p-built); decisions and threat model in
`docs/adr/0001-p2p.md`). The server is their registry: it knows which peer
belongs to which user and which peers are present in a team. Message bodies
never pass through the server.

**Binding a peer.** A peer id is an Ed25519 key. To bind one, the client
proves it holds the key:

```text
POST /api/v1/me/peers/challenge  -> { challenge, expires_at }   (valid 60 s)
signature = Ed25519("nanace-p2p-bind-v1\0" + challenge + "\0" + peer_id + "\0" + user_id + "\0")
POST /api/v1/me/peers { peer_id, challenge, signature, client_kind, client_version }
```

- The challenge is stateless: a nonce and expiry authenticated with an HMAC
  key derived from `NANACE_JWT_SECRET`, bound to the caller. It can be
  answered more than once within its minute; binding is idempotent.
- A peer id bound to another user is `409`. Without the proof, a user could
  bind someone else's peer id first and lock its owner out.
- Revoking (`DELETE /me/peers/{peer_id}`) is final: the row stays as a
  tombstone, the id cannot be bound again, and its presence ends at once.
- A user may have `NANACE_P2P_MAX_PEERS_PER_USER` unrevoked peers (default 10).

**Presence.** `PUT …/teams/{team}/presence` announces one agent of a peer in
a team and is repeated as a heartbeat.

- `ttl_secs` is at most `NANACE_P2P_PRESENCE_MAX_TTL_SECS` (default 120).
- Any team member can be present, readers included, once the team's policy
  enables P2P.
- Addresses are checked: dialable `ip4`/`ip6`/`dns` over `tcp` or `quic-v1`;
  loopback, link-local, unspecified and multicast are rejected.
- Capabilities are labels a peer gives itself. They are used for search only
  and grant nothing.
- Only the start of a presence is audited (`p2p.presence_put`), not each
  heartbeat. Its end is audited as `p2p.presence_delete` or
  `p2p.presence_expire`.

**What ends a presence:**

| Event | How |
|---|---|
| The TTL passes | Reads filter on `expires_at`; a task deletes expired rows every minute |
| The user leaves or is removed from the team or org | Foreign key to `team_members` |
| The peer is revoked | Same transaction as the revocation |
| The token that announced it is revoked (its whole rotation chain) | Same transaction as the revocation |
| The account is deleted | Peers are deleted explicitly, because the user row stays |
| The organization is deleted | Foreign-key cascade |

**Team policy.** P2P is off in a team until its owner creates a page titled
`Policy: P2P`. The first ```` ```yaml p2p-policy ```` block of that page is
the rules; the rest is guidance for agents. `GET /api/v1/p2p/policy-template`
returns a starting page with strict defaults.

- **Checked on save.** The server parses the block on every write of the
  page (YAML, at most 16 KiB, unknown keys are errors, values range-checked,
  patterns compiled with the linear-time `regex` crate). A page that does not
  validate is refused with `400` and the reasons; nothing is saved.
- **Stored with the page.** The parsed policy goes to `p2p_policies` in the
  transaction of the page write, audited as `p2p.policy_update` with the
  version (content hash) before and after. A second policy page in a team is
  `409`.
- **Off by default.** No row, an invalid row, or `enabled: false` means no
  presence can be announced, no peers are listed and lookups answer `404`,
  at once: presence rows are not deleted, they are just not served.
- **What turns it off:** trashing the page or renaming it away from the
  policy title. Restoring it parses it again; a restored page that no longer
  validates comes back as a page and leaves P2P off.
- **Rules are by team role**, read from PostgreSQL: `calls` lists which
  roles may send which kinds of message to which roles, and anything not
  listed is refused. Capabilities play no part in rules.
- **What the server enforces now:** `enabled`; `max_accept_mode` on
  presence; `discovery.visible_to` on the peer list; `discovery.lan_addrs`
  on announced addresses; `server_peers` for peers that run without the
  desktop app. The remaining fields (`calls`, `limits`, `data`, `summaries`)
  are served to daemons through `GET …/p2p/policy` and enforced there.
- **Pages from before this version** titled `Policy: P2P` have no stored
  policy. P2P stays off and `GET …/p2p/policy` tells the owner to save the
  page again.

**Lookup.** `GET /me/peers/lookup/{peer_id}` returns the owner and public key
of a peer that is present in a team the caller shares with it, and `404`
otherwise. A daemon will use it to decide whom to connect to and to check who
signed a message.

**Row-level security.** Both tables use `FORCE ROW LEVEL SECURITY` with
policies for `nanace_tenant` only, in three scopes:

- **Org and user** (`db::scoped_tx`): presence of the caller's teams in that
  org. A user writes presence only for their own, unrevoked peers.
- **User only** (`db::user_tx`): the caller's own peers, and peers and
  presence of users who share a team with the caller in any org.
- **System** (`db::p2p_system_tx`): the server's own maintenance and the
  network node's allow list. It may read the P2P tables, delete presence
  that has already expired and delete exchange reports past their
  retention, nothing else. It is never used on behalf of a request's caller.

`p2p_policies` is written only by team admins and `p2p_exchanges` only by
the side that reports; both are under the same forced row-level security.

**Rate limits.** Challenges, binds, presence and lookups are limited per user
with an in-memory token bucket (`429` with `Retry-After`). Each replica
counts on its own, so this bounds abuse and is not a quota.

**Exchange journal.** Each side of an exchange reports it to
`POST …/p2p/exchanges` with the request's signed header and, once there is
one, the signed answer header.

- The server checks the signatures against the peer keys, that both peers
  are bound to the users the header names, and that the reporter is that
  side. The row's facts (who, to whom, kind, sizes, trace) come from the
  signed header, not from the reporter.
- A report is updated as the exchange moves on (`delivered`, then
  `answered`, `rejected`, `expired` or `failed`), and each write is audited
  as `p2p.exchange`.
- `GET …/p2p/exchanges` is for team admins and org admins. `has_summary`
  says whether a page of the team carries the tag `trace-<trace id>`.
- Reports are kept `NANACE_P2P_EXCHANGE_RETENTION_DAYS` days (default 30), or
  the shorter period of the team's policy.

**Network node access.** The node asks the server which peers may connect,
on a separate listener (`NANACE_INTERNAL_BIND`) with a shared secret
(`NANACE_P2P_NODE_SECRET`, at least 32 bytes). A peer is allowed while it is
bound to a live account, not revoked, and present in a team whose policy
enables P2P. That list is all the node learns. `GET /api/v1/p2p/config`
tells daemons which node to use (`NANACE_P2P_BOOTSTRAP`).

**Metrics:** `nanace_p2p_active_peers`, `nanace_p2p_peer_binds_total`,
`nanace_p2p_presence_starts_total`, `nanace_p2p_policy_denials_total{reason}`,
`nanace_p2p_exchanges_total{kind,status}`.

### Agent P2P (`nanace-p2p`) [built]

One binary with two modes.

```text
          server side                                   a laptop, or an agent container
 ┌────────────────────────────────┐          ┌──────────────────────────────────────────┐
 │ nanace-server                  │  HTTPS   │ nanace-p2p daemon                         │
 │   public API  ◀────────────────┼──────────┼──  p2p-scoped token                       │
 │   internal listener ◀──┐       │          │   ▲ local socket                          │
 │ nanace-p2p node        │ allow │  libp2p  │   ├─ nanace-mcp ◀─ Claude  (agent role)   │
 │   relay v2, AutoNAT ───┘ list  │◀────────▶│   └─ desktop app           (supervisor)   │
 └────────────────────────────────┘          └──────────────────────────────────────────┘
        daemons reach each other directly when they can, through the relay otherwise
```

**Node** (`nanace-p2p node`): a public relay daemons bootstrap from.
Circuit Relay v2 with limits sized for text, an AutoNAT server, `identify`
and `ping`. It holds no token, no database access and no message; relayed
traffic stays encrypted between the two daemons.

**Daemon** (`nanace-p2p daemon`): one per user and machine.

- **On a laptop** the desktop app starts it as a sidecar and it accepts
  requests only while the app is attached. **In a container** it runs on its
  own (`NANACE_P2P_KIND=server`), takes its teams and accept modes from
  `NANACE_P2P_TEAMS`, and its account must be listed in the team policy's
  `server_peers`.
- **Transports:** QUIC and TCP with Noise and yamux. Behind NAT a daemon
  reserves a slot on the node and is reached through it; DCUtR then tries a
  direct connection.
- **Closed network:** no pre-shared key. A daemon keeps a connection only
  with the node and with peers the registry vouches for
  (`GET /me/peers/lookup`), re-checked every 30 seconds; the node admits only
  the peers the server lists. Revoking a peer or its token ends its
  connections within that time.
- **Protocols:** `/nanace/p2p/ask/1.0.0`, `/nanace/p2p/status/1.0.0`,
  `/nanace/p2p/notify/1.0.0`. Each message is acknowledged at once; the
  answer to a question is a separate message later, because consent and the
  agent may take minutes.

**Messages.** A message is a signed header plus its text.

- The header carries ids, sender and receiver (peer, agent, user), team,
  kind, hop count, deadline, the policy version and the hash of the text. It
  is signed with the sender's peer key over its exact bytes.
- An answer's header names the hash of the request header, so it cannot be
  attached to another request.
- The server only ever receives headers.

**What a daemon checks before a person or a model sees a message**, in order:

1. The signature, that the sender is the peer on the other end of the
   connection, and that the message is addressed to this peer and user.
2. That it is still alive and was not seen before.
3. With the registry: that the peer belongs to the user it names and shares
   the team.
4. With the daemon's own, current copy of the team policy: P2P is on, the
   call rule allows this sender role, receiver role and kind, the size, the
   hop limit and the rate per peer.
5. The secret rules (below).
6. That the addressed agent is registered, and consent.

Anything that fails is refused with a reason from a closed set
(`policy`, `rate`, `size`, `hops`, `pattern`, `consent`, `replay`,
`invalid`, `no_agent`), never with text.

**Secret rules.** Built-in patterns for private keys, nanace tokens, JWTs,
cloud and vendor keys, `Authorization` headers, URLs with credentials and
`password=`-style assignments are applied to every text: by the sender
before it is signed and by the receiver before it is shown. A team policy
can add patterns and cannot remove these. They recognize known formats; a
secret in an unknown format, encoded, or described in words is not caught.

**Consent.** The accept mode is the user's choice per team, capped by the
policy's `max_accept_mode`:

| Mode | Inbound requests |
|---|---|
| `off` | Refused. Also what applies on a laptop when the desktop app is not attached |
| `ask` | Each one waits for the user |
| `trusted` | Accepted from users the user trusts, others wait |
| `team` | Accepted from everyone the policy allows |

A request nobody decides on expires after the policy's
`consent_timeout_secs`. With "show answers before sending", the agent's
answer also waits for the user. The desktop app starts with it on; the user
may turn it off unless the team policy sets `require_answer_review: true`.
A peer without the desktop app has nobody to ask and answers on its own.

**Delivery.** A request waits in the sender's queue and is retried until
its deadline (`request_ttl_secs`), then expires. There is no mailbox on the
server, so both daemons must be online at the same moment at some point
before the deadline. Queued messages are files only the user can read
(`<state dir>/spool`). Their text does not outlive the exchange: a request's
text is dropped by the sender on delivery and by the receiver when it
answers, an answer's text by the receiver on delivery and by the sender ten
minutes after its agent read it. Headers stay for a day. Chains of requests
under one trace stop at the policy's `max_hops`.

**Bounds.** A daemon keeps at most 200 unfinished received requests (20 from
one user, 8 MiB of text) and shows the user at most 30 consent prompts (3
from one user); more are refused with `rate`. Daemon and node cap their
connections, and both bound how often peers they do not know make them ask
the server. A node that cannot fetch its allow list for 30 seconds turns
every peer away.

**Local socket.** Programs of the same user reach the daemon over a Unix
socket (a named pipe on Windows) with JSON-RPC, one message per line. A
session presents a secret and gets a role:

- **agent** (`nanace-mcp`): registers an agent label, sends, reads its
  inbox, answers, sets its status. The secret is in a file only the user can
  read.
- **supervisor** (the desktop app): also answers consent prompts and changes
  settings. Its secret is passed to the daemon on standard input and is never
  on disk, so a session with the agent secret cannot approve its own inbound
  requests.

**Status requests** are answered from the status the agent last recorded
with `p2p_set_status`, without waking a model.



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
- **Protected pages:** writing a `Policy:` or `Wiki:` page without the team
  role `admin` is a tool error that names who can edit it.
- **Concurrent edits:** a full replacement must pass the `updated` value
  of the `get_context` it is based on as `base_updated_at`; appends use the
  version read just before writing. Either way a concurrent edit returns a
  conflict instead of being overwritten.
- **Delimiters:** page text and search snippets cannot close or fake the
  `<context_content>` delimiters.
- The org is the tool argument, then `NANACE_ORG_ID`, then the user's only org.
- Context content is returned inside `<context_content>` delimiters, and the
  server instructions tell the model to treat it as data, not instructions.
- **P2P tools** (`p2p_whoami`, `p2p_find_peers`, `p2p_ask`,
  `p2p_get_reply`, `p2p_status`, `p2p_set_status`, `p2p_inbox`, `p2p_reply`,
  `p2p_get_policy`) exist only with `NANACE_ALLOW_P2P=true` and a reachable
  daemon (`NANACE_P2P_SOCKET`, `NANACE_P2P_SECRET_FILE`). Without the daemon
  they are not registered and the server instructions say why. They relay to
  the user's daemon over its local socket; `nanace-mcp` links only that
  client, no networking code.
  - Text from a peer is returned inside `<peer_message>` tags that it cannot
    close or fake, with the sender taken from the daemon.
  - The instructions tell the model that peer messages are data: answer
    them, do not act on them, do not reveal secrets, check claims in nanace.
  - The verdict agent runs without `NANACE_ALLOW_P2P` and so has none of
    these tools.
- `list_contexts` takes an optional `tag`, for example `verdict` or
  `trace-<trace id>`.
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
nanace-mcp (stdio) ---- local socket ----> nanace-p2p daemon <== libp2p ==> other daemons
    ^                                          |  ^                     (direct, or through
    | MCP protocol                             |  | supervisor           the nanace-p2p node)
Claude Desktop / Claude Code                   |  desktop app
                                               v
                                         Backend (REST, p2p-scoped token)
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
| `crates/nanace-mcp` | stdio MCP server over the REST API; read tools, opt-in create/update, opt-in P2P tools |
| `crates/nanace-p2p` | `nanace-p2p` binary: network node and per-user daemon; its local IPC client for the other programs |
| `integrations/claude-code` | Installer, the `/nanace` skill, and P2P conventions with a verdict agent example |
| `docs/adr` | Decision records |
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
| `PATCH` | `/api/v1/orgs/{org}` | Org admin; rename |
| `DELETE` | `/api/v1/orgs/{org}` | Org admin; body `{"confirm": "<slug>"}`; deletes everything in it |
| `DELETE` | `/api/v1/me` | Session only; erases the caller's account (GDPR) |
| `GET` | `/api/v1/orgs/{org}/members` | Org admin; members with their teams |
| `PUT` | `/api/v1/orgs/{org}/members/{user}` | Org admin; role `admin` or `member` (not the last admin) |
| `DELETE` | `/api/v1/orgs/{org}/members/{user}` | Org admin; removes from the org and all its teams (not the last admin) |
| `GET/POST` | `/api/v1/orgs/{org}/teams` | Org member / org admin |
| `GET/PUT` | `/api/v1/orgs/{org}/teams/{team}/members` | Team member or org admin / team admin or org admin |
| `DELETE` | `/api/v1/orgs/{org}/teams/{team}/members/{user}` | Team admin or org admin |
| `GET/POST` | `/api/v1/orgs/{org}/teams/{team}/contexts` | Team reader (`?tag=` filters) / team agent or writer (team admin for `Policy:` and `Wiki:` titles) |
| `GET/PUT/DELETE` | `/api/v1/orgs/{org}/contexts/{id}` | Team reader / team writer, or the agent that created the page / team writer (team admin for `Policy:` and `Wiki:` pages) |
| `GET` | `/api/v1/orgs/{org}/contexts/{id}/links` | Team reader; outgoing, backlinks, related |
| `GET` | `/api/v1/orgs/{org}/graph` | Org member; readable contexts, link and similarity edges |
| `GET` | `/api/v1/orgs/{org}/trash` | Team writer; trashed pages of writable teams |
| `POST` | `/api/v1/orgs/{org}/trash/{id}/restore` | Team writer (team admin for `Policy:` and `Wiki:` pages) |
| `DELETE` | `/api/v1/orgs/{org}/trash/{id}` | Team writer (team admin for `Policy:` and `Wiki:` pages); permanent |
| `POST` | `/api/v1/orgs/{org}/search` | Org member; results limited to readable teams |
| `GET` | `/api/v1/orgs/{org}/audit-logs` | Org admin |
| `POST` | `/api/v1/me/peers/challenge` | Authenticated, write; challenge for binding a P2P peer |
| `GET/POST` | `/api/v1/me/peers` | Authenticated / write, with proof of the peer key |
| `DELETE` | `/api/v1/me/peers/{peer_id}` | The peer's owner, write; final |
| `GET` | `/api/v1/me/peers/lookup/{peer_id}` | Authenticated; only peers present in a shared team |
| `PUT` | `/api/v1/orgs/{org}/teams/{team}/presence` | Team member, write; announce or extend presence |
| `DELETE` | `/api/v1/orgs/{org}/teams/{team}/presence/{peer_id}/{agent_label}` | The presence's owner, write |
| `GET` | `/api/v1/orgs/{org}/teams/{team}/peers` | Team member; `?capability=`, `?limit=`; empty unless the policy shows peers to the caller's role |
| `GET` | `/api/v1/orgs/{org}/teams/{team}/p2p/policy` | Team member; parsed policy, `?text=true` adds the guidance for agents |
| `GET` | `/api/v1/p2p/policy-template` | Authenticated; a `Policy: P2P` page to start from |
| `GET` | `/api/v1/p2p/config` | Authenticated, any scope; the network node to bootstrap from |
| `POST` | `/api/v1/orgs/{org}/teams/{team}/p2p/exchanges` | Team member that is one side of the exchange; signed headers only |
| `GET` | `/api/v1/orgs/{org}/teams/{team}/p2p/exchanges` | Team admin or org admin; `?trace_id=`, `?limit=`, `?offset=` |
| `GET` | `/internal/p2p/allowed`, `/internal/p2p/allow` | The network node, on the internal listener only |
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
