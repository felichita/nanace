---
name: adversarial-validation
description: Review nanace diffs or designs for explicit adversarial requests, high-risk changes under the repository review policy, or substantial PR reviews. Skip ordinary questions, diagnosis, planning, status, routine low-risk implementation, and prose with no execution effect.
---

# nanace Adversarial Validation

Use the [repository risk tiers and review shape](../../references/adversarial-validation.md). This skill
routes a review to nanace-specific probes without loading unrelated domains.

## Select Lenses

Read only the references required by the diff:

| Lens | When to read |
|---|---|
| [Correctness](references/correctness.md) | Every non-exempt adversarial review |
| [Simplicity](references/simplicity.md) | Mechanical/standard changes and production growth |
| [Test coverage](references/test-coverage.md) | Behavior or test changes |
| [Security](references/security.md) | Authn/authz, RBAC, RLS, tenancy, secrets, parsing, MCP, sync |
| [Consistency/concurrency](references/consistency.md) | PostgreSQL/Qdrant write ordering, transactions, locks, async shared state |
| [Compatibility](references/compatibility.md) | API shape, migrations, Qdrant payload/collections, embedding model, JWT claims |
| [Performance](references/performance.md) | Embedding, search, request hot paths, queries, pool usage |

Do not read all references as a precaution. A path name alone is insufficient;
the changed behavior must touch the lens's domain.

For a dedicated security audit, use `security-advisory-lessons` instead of
loading it automatically during every adversarial review.

## Review Protocol

1. Freeze the exact final diff/head (or the design under review) and list the
   selected lenses.
2. Run the review shape required by the repository risk tier.
3. For each selected lens, either report a concrete finding or a null verdict
   naming the attacks performed.
4. Apply root `AGENTS.md`'s finding standard. Test each candidate against callers,
   existing coverage, and invariants before accepting it; an adversarial role
   does not have to produce a defect.
5. Fix or rebut supported findings with code-path, test, or invariant evidence.
6. After a non-trivial edit, rerun only lenses affected by that edit against the
   new exact diff.

Do not turn a null verdict into a long checklist. Record concise evidence that
the relevant failure classes were attacked, then stop under the root completion
rule. Keep the required per-lens verdicts for high-risk PRs.
