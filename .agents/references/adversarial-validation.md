# Adversarial Review Shape

Use when root `AGENTS.md` triggers adversarial validation or for a substantial PR
review. Paths below are repository-relative. The root finding standard and
completion rule apply; selecting a lens does not require finding a defect.

Risk and review shape:

- **Exempt:** documentation, comments, formatting, or typos with no runtime,
  build, test, or agent-execution effect.
- **Mechanical:** renames, moves, test/tooling-only changes, and agent-rule
  changes. Run correctness and simplicity lenses.
- **Standard:** localized behavior changes. Run one integrated final-diff pass
  covering correctness, simplicity, and test coverage; add only domain lenses
  matched by the diff.
- **High risk / substantial PR review:** high risk includes authentication
  (JWT, OAuth, dev login), authorization and RBAC, RLS policies and
  `db::scoped_tx`, tenant isolation in Qdrant filters or collection naming,
  migrations, audit logging, Google Groups sync, MCP tool exposure, secrets
  handling, and the PostgreSQL/Qdrant write ordering. Cover all applicable
  lenses using exactly two independent reviewers when delegation is
  explicitly authorized. Split the lenses between them. Otherwise perform two
  fresh sequential passes.

Available domain lenses are security, consistency/concurrency, compatibility,
and performance. Select `.agents/skills/adversarial-validation/SKILL.md` for an
explicit adversarial request, a high-risk change, or a substantial PR review;
then read only its matching role references.
