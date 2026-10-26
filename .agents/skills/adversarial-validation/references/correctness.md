# Correctness Lens

Attack the changed behavior, not every subsystem in the repository.

- Trace new error paths to the caller. Inject the ignored/wildcard variants and
  verify they cannot become success, not-found, or a plausible default.
- Exercise zero/empty/missing, maximum, and exact-boundary inputs for every
  changed count, size, page limit, or optional value (`limit` 0/1/max/max+1,
  empty content, content at exactly 256 KiB, `n` chunks vs `n+1`).
- For pagination, test equal `updated_at` values and stable tie-breaking.
- For chunking, verify no text is lost, every chunk fits the limit, cuts land on
  UTF-8 boundaries, and fenced code does not create sections.
- For context updates, verify the stored chunk count, `embedding_hash`, and the
  Qdrant point set agree after shrink, growth, and tag-only edits.
- For membership changes, verify the effect is visible on the next request
  with an already-issued token.
- For agent rules/skill routers, test a trigger matrix covering ordinary
  inquiry, low-risk implementation, explicit review, high-risk code, PR
  creation, and post-PR monitoring. Each case must select only the intended
  workflow and retain required safety/authorization boundaries.

Null verdicts name only the probes relevant to the diff.
