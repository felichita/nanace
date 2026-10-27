-- Trash: deleting a context moves it here; it can be restored or purged.
--
-- Trashed rows live outside `contexts`, so every read path (listing, search,
-- graph, MCP) ignores them without extra filters. Only team writers and
-- admins see, restore, or purge a team's trash.

CREATE TABLE context_trash (
    id          uuid PRIMARY KEY,
    org_id      uuid NOT NULL,
    team_id     uuid NOT NULL,
    title       text NOT NULL,
    content     text NOT NULL,
    tags        text[] NOT NULL,
    created_by  uuid NOT NULL REFERENCES users (id),
    created_at  timestamptz NOT NULL,
    updated_by  uuid NOT NULL REFERENCES users (id),
    updated_at  timestamptz NOT NULL,
    deleted_by  uuid NOT NULL REFERENCES users (id),
    deleted_at  timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (org_id, team_id) REFERENCES teams (org_id, id) ON DELETE CASCADE
);
CREATE INDEX context_trash_org_deleted_idx ON context_trash (org_id, deleted_at DESC);

GRANT SELECT, INSERT, DELETE ON context_trash TO nanace_tenant;

ALTER TABLE context_trash ENABLE ROW LEVEL SECURITY;
ALTER TABLE context_trash FORCE ROW LEVEL SECURITY;

CREATE POLICY context_trash_select ON context_trash FOR SELECT TO nanace_tenant
    USING (
        org_id = nanace_current_org()
        AND EXISTS (
            SELECT 1 FROM team_members m
            WHERE m.team_id = context_trash.team_id AND m.user_id = nanace_current_user()
              AND m.role IN ('writer', 'admin')
        )
    );

CREATE POLICY context_trash_insert ON context_trash FOR INSERT TO nanace_tenant
    WITH CHECK (
        org_id = nanace_current_org()
        AND deleted_by = nanace_current_user()
        AND EXISTS (
            SELECT 1 FROM team_members m
            WHERE m.team_id = context_trash.team_id AND m.user_id = nanace_current_user()
              AND m.role IN ('writer', 'admin')
        )
    );

CREATE POLICY context_trash_delete ON context_trash FOR DELETE TO nanace_tenant
    USING (
        org_id = nanace_current_org()
        AND EXISTS (
            SELECT 1 FROM team_members m
            WHERE m.team_id = context_trash.team_id AND m.user_id = nanace_current_user()
              AND m.role IN ('writer', 'admin')
        )
    );

-- Restoring keeps the original author: a context may be inserted with another
-- user's `created_by` only when it is coming back from the trash with that
-- same author (the trash row is deleted after the insert, in one transaction).
DROP POLICY contexts_insert ON contexts;
CREATE POLICY contexts_insert ON contexts FOR INSERT TO nanace_tenant
    WITH CHECK (
        org_id = nanace_current_org()
        AND updated_by = nanace_current_user()
        AND (
            created_by = nanace_current_user()
            OR EXISTS (
                SELECT 1 FROM context_trash t
                WHERE t.id = contexts.id AND t.org_id = contexts.org_id
                  AND t.team_id = contexts.team_id AND t.created_by = contexts.created_by
            )
        )
        AND EXISTS (
            SELECT 1 FROM team_members m
            WHERE m.team_id = contexts.team_id AND m.user_id = nanace_current_user()
              AND m.role IN ('writer', 'admin')
        )
    );
