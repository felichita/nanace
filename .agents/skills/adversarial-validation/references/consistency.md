# Consistency and Concurrency Lens

PostgreSQL is the source of truth; Qdrant is a derived index. Attack every
interleaving where they can disagree.

- Write ordering: create/update upsert vectors before commit (a Qdrant failure
  aborts the transaction); delete commits before removing vectors. For a
  changed path, simulate failure after each step and state what the user sees.
  Orphan points are acceptable only because search re-checks PostgreSQL.
- Concurrent updates of one context must be serialized (`FOR UPDATE`) so the
  last committed PostgreSQL row and the Qdrant point set come from the same
  writer.
- Point ids are deterministic per `(context_id, chunk_index)`; stale points
  above the new chunk count must be deleted after the upsert, never before.
- Audit entries are written in the same transaction as the change they
  describe. A committed change without an audit row is a defect.
- Do not hold a transaction or pooled connection across long CPU work or
  external calls without a reason; measure what is held.
- `std::sync::Mutex` guards are never held across `.await`; blocking embedding
  work runs in `spawn_blocking`.
- Cancellation: a dropped request future between the Qdrant write and commit
  rolls the transaction back; confirm the leftover state is one of the
  acceptable states above.
- Migrations that rewrite data must be safe to rerun after a partial failure.
