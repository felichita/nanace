# nanace Threat Patterns

Each section lists the boundary, the attacks to trace, and where the defense
lives. Read only the sections matching the diff.

## Authentication and Tokens

- Forged or altered JWTs: algorithm pinned to HS256, signature, `iss`, `aud`,
  `exp` and `sub` all required (`crates/nanace-server/src/auth.rs`).
- Weak secrets: `NANACE_JWT_SECRET` shorter than 32 bytes must refuse startup.
- Dev login reachable in production: `/auth/dev-login` must 404 unless
  explicitly enabled, and the server must warn loudly when it is enabled.
- Google OAuth (when added): verify the ID token signature, `aud` (our client
  id), `iss`, `exp`, `email_verified`, and the hosted domain if restricted.
  Link accounts by `google_sub`, not by email alone after first link.
- Tokens grant identity only; a token for a removed member must lose access on
  the next request.

## Authorization and RBAC

- Path confusion: team id from another org combined with our org id; context
  id from another team; member id from another team. Every lookup must bind
  both org and team.
- Role escalation: team admin granting org admin; writer granting roles;
  self-add to a team; role strings outside the enum.
- Org admin implicit content access: org admins manage membership only; any
  path that returns team content to a non-member org admin is a bypass.
- Response differences between "not found" and "forbidden" that reveal
  existence across teams or orgs.

## Tenant Isolation (PostgreSQL RLS)

- A `contexts` query outside `db::scoped_tx` (should see nothing because of
  FORCE RLS; if it sees rows, the role is superuser or BYPASSRLS).
- Policies weakened, added for other roles, or `nanace_tenant` granted
  privileges beyond what handlers need.
- `set_config` values taken from the request instead of the authenticated
  user and the authorized org.
- Superuser or BYPASSRLS deployment credentials (startup warning, tests refuse).
- SECURITY DEFINER functions or views over `contexts` that bypass RLS.

## Vector Store (Qdrant)

- Filters built from request-supplied team ids without intersecting with
  PostgreSQL memberships.
- Missing `org_id` filter, or collection name derived from anything but the
  authorized org id.
- Returning payload `text` for a hit without the PostgreSQL re-check (stale
  points after delete or membership removal).
- Qdrant exposed beyond loopback/private network without an API key.

## Input Handling

- Missing `deny_unknown_fields`, length limits, or validation on new fields.
- Oversized bodies (JSON limit), huge tag lists, pathological markdown for the
  chunker (very long lines, many headings, multibyte text).
- SQL built with `format!` instead of bound parameters.

## Secrets and Logging

- Tokens, database URLs, JWT secrets, context content, or search queries in
  logs, error responses, panics, or `Debug` output.
- `.env` or credentials committed; development passwords reused in deployment.

## MCP and Prompt Injection

- Context markdown is attacker-controllable text that Claude reads. MCP tools
  must take tenant and team scope from the authenticated session, never from
  tool arguments that stored content could influence.
- MCP tool results must pass the same RBAC and RLS checks as the HTTP API.

## Google Groups Sync

- Sync must only touch `source = 'google_sync'` memberships; it must not
  delete manual memberships or grant roles beyond the mapping.
- Group-to-team mapping must be org-scoped; a group email configured in two
  orgs must not merge them.
- Sync credentials (service account keys) are secrets with minimal scopes.

## Audit Log

- State changes and content reads without an audit row in the same
  transaction; audit rows readable by non-admins or across orgs; audit
  details containing content or secrets.
