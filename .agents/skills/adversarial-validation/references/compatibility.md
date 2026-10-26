# Compatibility Lens

- API responses: removing or renaming a JSON field, changing a status code, or
  changing 404-vs-403 semantics breaks clients (frontend, MCP tools). Additive
  fields are compatible.
- Request bodies use `deny_unknown_fields`; adding a required field breaks old
  clients, an optional field with a default does not.
- Migrations are append-only. Never edit an applied migration file; sqlx
  checksums will fail on existing databases.
- Adding a NOT NULL column needs a default or a backfill in the same migration.
- Qdrant payload keys (`org_id`, `team_id`, `context_id`, `chunk_index`,
  `text`) and the collection naming scheme are a storage contract. Changing
  them requires a re-index plan for existing collections.
- Changing the embedding model, its dimensions, the query prefix, or the chunk
  text template invalidates stored vectors. `model_id` must change with it, and
  the change needs a re-index path.
- JWT claim changes (issuer, audience, `sub` type) invalidate issued tokens;
  verify that is intended.
