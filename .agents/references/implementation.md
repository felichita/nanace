# Implementation Rules

Applies when changing code or running artifact-heavy work. Paths below
are repository-relative. Read only the relevant sections during read-only review.

## Worktree and Disk Hygiene

- Start implementation from the latest default branch and confirm the
  requested change is not already present.
- An existing clean, isolated task worktree is sufficient. Create another
  worktree only when the current checkout is shared, dirty with unrelated work,
  or belongs to another task.
- Never commit from a shared checkout.
- Use a task-specific branch named `<type>/<topic>`, such as `fix/...`,
  `feat/...`, `test/...`, or `docs/...`, unless the user specifies a name.
- Do not include agent, tool, contributor, account, or organization names in
  branch names.
- Push to the user-requested remote or the repository's configured push remote.
  Do not hard-code or infer a remote from an account name.
- Check free space before artifact-heavy builds or model downloads.
- Remove only task-owned temporary/build artifacts. Never delete another task's
  worktree or uncommitted data.
- At handoff, mention disk or cleanup details only when they affected execution
  or artifacts/worktrees remain intentionally.

## Change Style

- Preserve existing control flow unless changing it is required for correctness.
- Prefer a direct local edit over new files, wrappers, managers, or speculative
  abstractions.
- Add a helper only when it removes current duplication, names a real domain
  boundary, or isolates a non-trivial invariant.
- Remove an in-scope path superseded by the change. If compatibility requires it,
  adapt at the boundary to one canonical core and note the removal condition.
- Comments explain non-obvious invariants or reasons. Do not narrate code or
  record change history.
- Mention unrelated problems when useful; do not fix them in a narrow task.

## Reuse and Boundary Rules

- Before adding helpers, constants, fixtures, or wrappers, search the touched
  crate, `crates/nanace-core` (domain logic and validation), and relevant
  direct dependencies.
- Reuse requires matching semantics: normalization, error types, deadlines,
  durability, and compatibility must fit the call site. A narrowly named local
  helper is better than forced reuse with different semantics.
- Validate untrusted input at its trust boundary, then trust the validated type.
  Qdrant payloads, JWT claims, and data from external services (Google, MCP
  clients) remain untrusted at every consumer.
- Re-check authorization immediately before destructive actions such as
  delete or overwrite.
- Every new branch needs a concrete triggering input/state.
- Required values must return a typed error when absent or corrupt; do not use a
  default that converts corruption into a plausible result.
- Attach error context once where it is actionable. Map errors to `ApiError`
  at the handler boundary; never leak internal error text to clients.

## Rust Boundaries

- Production `unwrap`/`expect` must follow a type guarantee or checked invariant; explain only non-obvious guarantees.
- Public APIs do not return `Result<_, String>`. Library APIs use domain errors unless intentional erasure is part of the boundary contract; expose a stored inner error through `Error::source()`.
- Numeric conversions must not silently truncate. Use fallible conversions for untrusted values; validate finiteness, sign, and range before float-to-integer conversion. Clamp/saturate only when required by the domain.
- Do not add crate-root `#![allow(dead_code)]`.
- Database access uses runtime `sqlx::query*` with static SQL strings and bound
  parameters; never format values into SQL.
- Schema changes are new files in `crates/nanace-server/migrations/`; never
  edit an applied migration.

## Naming

Use Rust API naming: `SCREAMING_SNAKE_CASE` constants/statics, `snake_case`
functions/variables, and `PascalCase` types. Do not rename unrelated existing
violations.
