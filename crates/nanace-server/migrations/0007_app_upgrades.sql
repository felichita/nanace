-- Application upgrades: steps written in Rust that SQL cannot do, applied
-- once by `nanace-server migrate` (or at startup), in version order, like a
-- module version map. See src/upgrades.rs.
CREATE TABLE app_upgrades (
    version     integer PRIMARY KEY,
    name        text NOT NULL,
    applied_at  timestamptz NOT NULL DEFAULT now(),
    details     jsonb NOT NULL DEFAULT '{}'
);

-- Facts about the installation, such as which embedding model produced the
-- vectors in Qdrant (`embedding_model`), so a model change triggers a reindex.
CREATE TABLE system_state (
    key         text PRIMARY KEY,
    value       text NOT NULL,
    updated_at  timestamptz NOT NULL DEFAULT now()
);
