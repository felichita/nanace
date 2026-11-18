-- Team role `agent`, for service accounts that only agents use. Between
-- reader and writer: an agent reads the team, creates pages and edits the
-- pages it created. It does not edit other users' pages, trash, restore or
-- purge anything, or write `Policy:` and `Wiki:` pages.

ALTER TABLE team_members DROP CONSTRAINT team_members_role_check;
ALTER TABLE team_members ADD CONSTRAINT team_members_role_check
    CHECK (role IN ('reader', 'agent', 'writer', 'admin'));

-- The scoped user's role in a team, or null when not a member.
CREATE FUNCTION nanace_team_role(team uuid) RETURNS text
    LANGUAGE sql STABLE
    AS $$ SELECT m.role FROM team_members m WHERE m.team_id = team AND m.user_id = nanace_current_user() $$;

-- An agent inserts only its own pages: the restore branch needs a trash row,
-- and the trash is invisible to agents.
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
        AND nanace_team_role(team_id) IN ('agent', 'writer', 'admin')
        AND nanace_may_write_title(team_id, title)
    );

DROP POLICY contexts_update ON contexts;
CREATE POLICY contexts_update ON contexts FOR UPDATE TO nanace_tenant
    USING (
        org_id = nanace_current_org()
        AND (
            nanace_team_role(team_id) IN ('writer', 'admin')
            OR (nanace_team_role(team_id) = 'agent' AND created_by = nanace_current_user())
        )
        AND nanace_may_write_title(team_id, title)
    )
    WITH CHECK (
        org_id = nanace_current_org()
        AND updated_by = nanace_current_user()
        AND (
            nanace_team_role(team_id) IN ('writer', 'admin')
            OR (nanace_team_role(team_id) = 'agent' AND created_by = nanace_current_user())
        )
        AND nanace_may_write_title(team_id, title)
    );

-- Links follow the page they belong to.
DROP POLICY context_links_insert ON context_links;
CREATE POLICY context_links_insert ON context_links FOR INSERT TO nanace_tenant
    WITH CHECK (
        org_id = nanace_current_org()
        AND EXISTS (
            SELECT 1 FROM contexts c
            WHERE c.id = context_links.source_id AND c.org_id = context_links.org_id
              AND (
                  nanace_team_role(c.team_id) IN ('writer', 'admin')
                  OR (nanace_team_role(c.team_id) = 'agent' AND c.created_by = nanace_current_user())
              )
        )
    );

DROP POLICY context_links_delete ON context_links;
CREATE POLICY context_links_delete ON context_links FOR DELETE TO nanace_tenant
    USING (
        org_id = nanace_current_org()
        AND EXISTS (
            SELECT 1 FROM contexts c
            WHERE c.id = context_links.source_id
              AND (
                  nanace_team_role(c.team_id) IN ('writer', 'admin')
                  OR (nanace_team_role(c.team_id) = 'agent' AND c.created_by = nanace_current_user())
              )
        )
    );
