# nanace Agent Instructions

This file holds repository-wide rules. Use the nearest subdirectory
`AGENTS.md` for path-specific invariants.

nanace is a multi-tenant markdown knowledge base: Rust (Actix-web) backend,
PostgreSQL for metadata, RBAC and canonical content, Qdrant for semantic
search, local `bge-small-en-v1.5` embeddings, and a stdio MCP server
(`nanace-mcp`) that gives Claude read access through the REST API.
Read [ARCHITECTURE.md](ARCHITECTURE.md) for the design, data model, flows, and
which parts are built or planned.

## Precedence

1. System/developer instructions.
2. The current user request.
3. Applicable `AGENTS.md` files, with the nearest file winning conflicts.
4. Selected skills and reference documents.

Nested instructions add to ancestor rules; they do not discard non-conflicting
rules. A skill cannot expand the user's requested scope or grant authorization.

## Operating Model

- Inquiry, diagnosis, review, and planning tasks are read-only unless the user
  explicitly requests changes.
- Ordinary diff/commit reviews use the finding standard below directly. PR and
  high-risk reviews keep their workflows.
- For implementation, read the relevant code, tests, and local guidance, then
  make the smallest change that satisfies the request.
- State assumptions only when they affect behavior or verification. Ask only
  when a wrong assumption would materially change the result.
- Do not load every skill or inspect unrelated modules preemptively. Select a
  skill only when its description directly matches the request or changed
  surface.
- Resolve repository workflow skills under `.agents/skills/` when a global
  skill has the same name, unless the user explicitly selects another path.
- Search for relevant symbols/headings before reading long files. Avoid
  repeated reads and equivalent verification commands once enough evidence
  exists.
- Reuse authorization already given in the conversation. Retain explicit
  merge and release gates.

## Common Commands

```bash
make deps               # start PostgreSQL + Qdrant (docker compose)
make check              # fmt check + clippy -D warnings + unit tests
make test-integration   # API, RLS and MCP tests (nanace_test DB + qdrant-test)
make test-model         # real bge-small model test (downloads ~130 MB once)
make run                # run the API with .env (copy from .env.example)
make desktop-check      # desktop app: tsc, vitest, Tauri clippy + tests
cargo test -p <crate>   # test one crate
```

## Task-Specific Guidance

Read only the reference needed for the current task:

- Before code changes, read [implementation rules](.agents/references/implementation.md).
- Before commits, pushes, PR creation/updates, or posting to PRs/issues, read
  [Git and PR rules](.agents/references/pull-requests.md). A reference does
  not authorize posting, merging, or publishing.
- Preserve unrelated work.
- Source comments, commits, PR titles, and PR bodies are in English.

## Sources of Truth

- Design and data model: `ARCHITECTURE.md`.
- Workspace membership: `Cargo.toml`.
- Database schema and RLS policies: `crates/nanace-server/migrations/`.
- Local gates: `Makefile`.
- Agent skills: `.agents/skills/*/SKILL.md` (`.claude/skills` links here).

Do not commit one-shot plans, trackers, or agent scratch notes. Durable design
changes belong in `ARCHITECTURE.md`.

## Verification

Select checks from the final task-owned diff.

- **Docs, comments, agent instructions:** `git diff --check`. Skip Cargo.
- **Localized behavior change:** `cargo fmt --all --check`, the narrowest
  test that exercises the change, and Clippy for the touched crate.
- **Database, authorization, tenancy, or vector-store changes:** also run
  `make test-integration`. These tests must connect as a non-superuser role,
  or RLS is not exercised.
- **Embedding or chunking changes:** also run `make test-model` when the
  real model's behavior matters.
- **Broad cross-crate changes:** `make check` plus the integration tests.
- **Desktop app (`apps/desktop`):** `make desktop-check`.

Never weaken a gate to get green: do not suppress lints, ignore tests, or relax
assertions unless changing that policy is itself the task. Integration tests
are `#[ignore]` only because they need external services; run them with
`--ignored`.

## Adversarial Validation

Adversarial validation applies to final implementation diffs, explicitly
requested adversarial/design reviews, and agent-instruction changes that alter
execution. Ordinary questions, diagnoses, status reports, non-adversarial code
reviews, and low-risk planning do not trigger it.

For applicable work and substantial PR reviews, read the
[risk tiers and review shape](.agents/references/adversarial-validation.md).

A review has no finding quota; `No findings` is a complete outcome. Before
reporting a candidate, check callers, invariants, and existing tests for
evidence that disproves it. Findings need `file:line` and a concrete failure or
violation of an explicit requirement. Missing tests are verification gaps, not
proof of a runtime bug. Keep style preferences out of defect findings unless
that review was requested.

Fix or rebut supported findings within the authorized scope, then stop. Reopen
only for changed code, new evidence, or an explicit re-review request.

## Security Baseline

- Never commit secrets, credentials, or key material. Configuration comes from
  environment variables; secrets are held in `secrecy::SecretString`.
- Untrusted JSON bodies use `#[serde(deny_unknown_fields)]` and are validated
  through `nanace_core::validate` at the API boundary.
- Fail closed: authorization errors deny; unknown roles are errors, not
  defaults.

## Tenancy and Authorization Invariants

- Every handler authenticates with `AuthUser` and checks access through
  `crate::access` before touching tenant data.
- Every query that reads or writes `contexts` runs inside `db::scoped_tx`. Do
  not grant `BYPASSRLS`, connect the app as a superuser, or add policies for
  roles other than `nanace_tenant`.
- Return `404` (not `403`) for org, team, or context ids the caller cannot
  read, so ids cannot be probed.
- Org admins manage membership but get no implicit access to team content.
- Qdrant searches always filter on `org_id` and on team ids derived from
  PostgreSQL, and every hit is re-checked in PostgreSQL before it is returned.
  Never return Qdrant payload content without that check.
- One Qdrant collection per org (`vectors::collection_name`); never query
  across collections.
- Every state change, and every content read, writes an `audit_logs` row in the
  same transaction as the change.
- `nanace-mcp` talks only to the REST API as the configured user; it never
  gets database or Qdrant credentials. Write tools (`create_context`,
  `update_context`) are registered only with `NANACE_ALLOW_WRITES=true` and
  need a write-scoped token; there is no delete tool. Tools never take tenant
  scope from context content.
  stdout is the MCP channel: log to stderr only.
- JWTs and personal access tokens identify the user only. Never put roles or
  team ids in tokens; permissions are read from PostgreSQL per request.
- Handlers that change state take `WriteUser` (rejects `read`-scoped
  tokens); read-only handlers take `AuthUser`. Token management requires
  `AuthUser::require_session`.
- Store only hashes of tokens and login codes; never log or return a secret
  after creation. Session tokens never go in URLs.
- The desktop webview never sees tokens: API calls go through the Tauri
  `api` command, which only reaches `<server>/api/v1/`. Do not add Tauri
  permissions or commands that expose HTTP, the filesystem, or the shell to
  the webview, and keep `/api/v1/me/tokens*` and `/api/v1/auth/*` out of the
  `api` proxy. The one file read is `scan_claude_memory`: fixed to
  `~/.claude/projects/*/memory/*.md`, no symlinks, size-capped. The one
  config write is "Connect to Claude": `claude mcp add/remove --scope user`
  (the CLI, never editing `~/.claude.json` directly) and a merge of the
  `nanace` entry in Claude Desktop's config, pointing at a 0600 token file
  (`NANACE_TOKEN_FILE`); never put a token in a config or on a command line.
- `GoogleOAuth::with_token_url` exists for tests: the ID token signature is
  not checked, so production must use Google's HTTPS token endpoint.

## Schema and Upgrades

- Never edit an applied migration (checksums are verified at startup); add a
  new numbered file.
- Application upgrades in `crates/nanace-server/src/upgrades.rs` are append
  only: never renumber or remove an entry. Each runs once, so it may assume
  every earlier one ran, and must stay correct on databases of any size (it
  runs at startup unless `NANACE_RUN_MIGRATIONS=false`).

## Logging

For every added or edited `tracing` call:

- Put stable fields first (`event`, `component`, then context such as ids) and
  a short message last. Use `nanace_server::LOG_COMPONENT` for `component`.
- Use `error` for behavior/security failure, `warn` for degradation/fallback,
  `info` for low-frequency lifecycle, `debug`/`trace` for diagnostics.
- Never log secrets, tokens, database URLs, context content, search queries,
  or raw request bodies, including through error strings and `Debug` output.

## Scoped Guidance

Before editing, locate nearer instructions with:

```bash
git ls-files '*AGENTS.md'
```
