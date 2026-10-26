# Test-Coverage Lens

- For every behavior claim, name the focused test/check that fails if the
  changed hunk is reverted. If none is practical, require the reason and
  residual risk.
- Confirm tests exercise the real production path and distinguish the intended
  behavior from the named regression. A success, `is_err()`, or no-panic check
  can be sufficient when that is the actual contract; require exact values,
  bytes, state, or error variants when those distinctions matter to the change.
- For new flags/modes, verify each branch and ask which test fails if the branch
  is inverted.
- For new error propagation, inject the failure and assert the caller observes
  it; mentally replacing `?`/`return Err` with success must break a test.
- Authorization changes need a negative test through the HTTP API (wrong team,
  wrong org, insufficient role, removed member) asserting the exact status.
- Tenancy changes also need a test below the application: a `scoped_tx`
  query that would leak without RLS. Integration tests must run as a
  non-superuser role, or RLS is silently skipped.
- Search changes assert the exact result ids (and absence of other teams'
  ids), not only a non-empty result.
- Use `HashingEmbedder` for deterministic tests; use the real model test only
  when ranking quality is the claim.
- Concurrency tests use readiness polling and avoid fixed sleeps.
- Avoid duplicate tests of the same production path and poison-value class.
  Boundary companions are distinct coverage: `n == max` vs `max + 1`.
- A focused test proves only the targets/features it builds. Add compilation or
  Clippy only for uncovered changed targets.
