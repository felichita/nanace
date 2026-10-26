-- Core schema: tenancy, RBAC, contexts, audit log.

CREATE TABLE organizations (
    id          uuid PRIMARY KEY,
    slug        text NOT NULL UNIQUE,
    name        text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE users (
    id             uuid PRIMARY KEY,
    email          text NOT NULL UNIQUE CHECK (email = lower(email)),
    display_name   text,
    google_sub     text UNIQUE,
    created_at     timestamptz NOT NULL DEFAULT now(),
    last_login_at  timestamptz
);

CREATE TABLE org_members (
    org_id      uuid NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    user_id     uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role        text NOT NULL CHECK (role IN ('member', 'admin')),
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (org_id, user_id)
);
CREATE INDEX org_members_user_idx ON org_members (user_id);

CREATE TABLE teams (
    id                  uuid PRIMARY KEY,
    org_id              uuid NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    slug                text NOT NULL,
    name                text NOT NULL,
    google_group_email  text,
    created_at          timestamptz NOT NULL DEFAULT now(),
    UNIQUE (org_id, slug),
    -- Target for composite FKs that pin a row's team to the same org.
    UNIQUE (org_id, id)
);

CREATE TABLE team_members (
    org_id      uuid NOT NULL,
    team_id     uuid NOT NULL,
    user_id     uuid NOT NULL,
    role        text NOT NULL CHECK (role IN ('reader', 'writer', 'admin')),
    source      text NOT NULL DEFAULT 'manual' CHECK (source IN ('manual', 'google_sync')),
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (team_id, user_id),
    FOREIGN KEY (org_id, team_id) REFERENCES teams (org_id, id) ON DELETE CASCADE,
    -- A team member is always an org member; removing from the org removes
    -- every team membership.
    FOREIGN KEY (org_id, user_id) REFERENCES org_members (org_id, user_id) ON DELETE CASCADE
);
CREATE INDEX team_members_user_idx ON team_members (user_id, org_id);

CREATE TABLE contexts (
    id               uuid PRIMARY KEY,
    org_id           uuid NOT NULL,
    team_id          uuid NOT NULL,
    title            text NOT NULL,
    content          text NOT NULL,
    tags             text[] NOT NULL DEFAULT '{}',
    embedding_hash   text NOT NULL,
    embedding_model  text NOT NULL,
    chunk_count      integer NOT NULL CHECK (chunk_count >= 0),
    created_by       uuid NOT NULL REFERENCES users (id),
    updated_by       uuid NOT NULL REFERENCES users (id),
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (org_id, team_id) REFERENCES teams (org_id, id) ON DELETE CASCADE
);
CREATE INDEX contexts_org_team_updated_idx ON contexts (org_id, team_id, updated_at DESC);

CREATE TABLE audit_logs (
    id             bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    org_id         uuid REFERENCES organizations (id),
    actor_user_id  uuid REFERENCES users (id),
    action         text NOT NULL,
    resource_type  text NOT NULL,
    resource_id    uuid,
    details        jsonb NOT NULL DEFAULT '{}',
    created_at     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX audit_logs_org_created_idx ON audit_logs (org_id, created_at DESC);

-- Row-level security.
--
-- Request handlers that touch contexts run inside a transaction that sets
-- `nanace.org_id` / `nanace.user_id` and switches to the `nanace_tenant` role
-- (see `db::scoped_tx`). Policies exist only for that role and the table forces
-- RLS on its owner, so any other path to `contexts` sees and changes nothing.

DO $$
BEGIN
    -- Deployments may pre-create the role and grant it (deploy/postgres/init.sql),
    -- in which case the migration user needs no CREATEROLE.
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'nanace_tenant') THEN
        CREATE ROLE nanace_tenant NOLOGIN NOINHERIT;
    END IF;
    IF NOT pg_has_role(current_user, 'nanace_tenant', 'MEMBER') THEN
        EXECUTE format('GRANT nanace_tenant TO %I', current_user);
    END IF;
END
$$;

GRANT SELECT ON organizations, users, org_members, teams, team_members TO nanace_tenant;
GRANT SELECT, INSERT, UPDATE, DELETE ON contexts TO nanace_tenant;
GRANT INSERT ON audit_logs TO nanace_tenant;

CREATE FUNCTION nanace_current_org() RETURNS uuid
    LANGUAGE sql STABLE
    AS $$ SELECT NULLIF(current_setting('nanace.org_id', true), '')::uuid $$;

CREATE FUNCTION nanace_current_user() RETURNS uuid
    LANGUAGE sql STABLE
    AS $$ SELECT NULLIF(current_setting('nanace.user_id', true), '')::uuid $$;

ALTER TABLE contexts ENABLE ROW LEVEL SECURITY;
ALTER TABLE contexts FORCE ROW LEVEL SECURITY;

CREATE POLICY contexts_select ON contexts FOR SELECT TO nanace_tenant
    USING (
        org_id = nanace_current_org()
        AND EXISTS (
            SELECT 1 FROM team_members m
            WHERE m.team_id = contexts.team_id AND m.user_id = nanace_current_user()
        )
    );

CREATE POLICY contexts_insert ON contexts FOR INSERT TO nanace_tenant
    WITH CHECK (
        org_id = nanace_current_org()
        AND created_by = nanace_current_user()
        AND updated_by = nanace_current_user()
        AND EXISTS (
            SELECT 1 FROM team_members m
            WHERE m.team_id = contexts.team_id AND m.user_id = nanace_current_user()
              AND m.role IN ('writer', 'admin')
        )
    );

CREATE POLICY contexts_update ON contexts FOR UPDATE TO nanace_tenant
    USING (
        org_id = nanace_current_org()
        AND EXISTS (
            SELECT 1 FROM team_members m
            WHERE m.team_id = contexts.team_id AND m.user_id = nanace_current_user()
              AND m.role IN ('writer', 'admin')
        )
    )
    WITH CHECK (
        org_id = nanace_current_org()
        AND updated_by = nanace_current_user()
        AND EXISTS (
            SELECT 1 FROM team_members m
            WHERE m.team_id = contexts.team_id AND m.user_id = nanace_current_user()
              AND m.role IN ('writer', 'admin')
        )
    );

CREATE POLICY contexts_delete ON contexts FOR DELETE TO nanace_tenant
    USING (
        org_id = nanace_current_org()
        AND EXISTS (
            SELECT 1 FROM team_members m
            WHERE m.team_id = contexts.team_id AND m.user_id = nanace_current_user()
              AND m.role IN ('writer', 'admin')
        )
    );
