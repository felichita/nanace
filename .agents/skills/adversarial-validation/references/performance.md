# Performance Lens

- Embedding is the dominant cost. Count model invocations per request; skip
  re-embedding when `embedding_hash` is unchanged; batch passages.
- Keep CPU-bound inference on the blocking pool; one request must not
  serialize unrelated requests longer than its own inference.
- Detect N+1 queries in list/search paths and per-item Qdrant calls in loops.
- New query shapes on `contexts`, `team_members`, or `audit_logs` need a
  matching index (`org_id`, `team_id`, `user_id`, `created_at`).
- Search over-fetches chunks (`CHUNK_OVERFETCH`) and groups per context; check
  that limits stay bounded and the PostgreSQL re-check is one query.
- Hot-path success logs stay at `debug`/`trace`.
- Claims of no impact on search or write latency need a measurement when the
  change touches embedding, chunking, or query shape.
