# Security Lens

Use `security-advisory-lessons` only for a dedicated security audit. For an
ordinary matched diff, attack these boundaries:

- Authentication: every non-public handler extracts `AuthUser`. JWT
  verification pins HS256, issuer, audience, and expiry; no `none`/algorithm
  confusion; secrets are at least 32 bytes.
- Dev login stays disabled unless `NANACE_DEV_LOGIN=true` and returns 404
  otherwise. Nothing else may issue tokens without verifying identity.
- Authorization: each handler calls `crate::access` with the exact org/team
  from the path and the minimum role for the action. Verify the team belongs
  to the org from the path (a valid team id in another org must 404).
- Tenancy: every `contexts` query runs in `db::scoped_tx`; RLS policies match
  the application checks; no superuser/BYPASSRLS connection; no new policy for
  roles other than `nanace_tenant`.
- Qdrant: filters always include `org_id` and team ids derived from
  PostgreSQL, never from the request alone; hits are re-checked in PostgreSQL
  before content is returned.
- Enumeration: unknown or unreadable org/team/context ids return 404, and
  error messages do not reveal whether the resource exists.
- Privilege escalation: team admins cannot grant org admin; members cannot
  add themselves to teams; role values are parsed strictly.
- Input: `deny_unknown_fields`, length limits, and `nanace_core::validate` on
  every new field; SQL uses bound parameters only.
- Secrets and logs: no tokens, database URLs, context content, or search
  queries in logs, errors, or `Debug` output.
- MCP and stored content: context markdown is untrusted data that reaches
  Claude; MCP tools must not let it change which tenant or team is queried.

Security findings distinguish unauthenticated compromise from a
low-privileged authenticated bypass.
