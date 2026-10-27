-- Wiki links between contexts (`[[Title]]`).
--
-- A link stores its target as `lower(title)` and is resolved at read time by
-- joining `contexts` on `lower(title)`. Both tables are under RLS, so a link
-- only resolves to contexts the reader can see, and links whose target does
-- not exist yet start resolving once it is created.
--
-- Contexts created before this migration get their links on their next edit.

CREATE TABLE context_links (
    org_id      uuid NOT NULL,
    source_id   uuid NOT NULL REFERENCES contexts (id) ON DELETE CASCADE,
    target_key  text NOT NULL CHECK (target_key = lower(target_key)),
    PRIMARY KEY (source_id, target_key)
);
CREATE INDEX context_links_target_idx ON context_links (org_id, target_key);
CREATE INDEX contexts_org_title_key_idx ON contexts (org_id, lower(title));

GRANT SELECT, INSERT, DELETE ON context_links TO nanace_tenant;

ALTER TABLE context_links ENABLE ROW LEVEL SECURITY;
ALTER TABLE context_links FORCE ROW LEVEL SECURITY;

-- The subqueries on `contexts` are themselves filtered by its RLS policies.
CREATE POLICY context_links_select ON context_links FOR SELECT TO nanace_tenant
    USING (
        org_id = nanace_current_org()
        AND EXISTS (SELECT 1 FROM contexts c WHERE c.id = context_links.source_id)
    );

CREATE POLICY context_links_insert ON context_links FOR INSERT TO nanace_tenant
    WITH CHECK (
        org_id = nanace_current_org()
        AND EXISTS (
            SELECT 1 FROM contexts c
            JOIN team_members m ON m.team_id = c.team_id
            WHERE c.id = context_links.source_id AND c.org_id = context_links.org_id
              AND m.user_id = nanace_current_user() AND m.role IN ('writer', 'admin')
        )
    );

CREATE POLICY context_links_delete ON context_links FOR DELETE TO nanace_tenant
    USING (
        org_id = nanace_current_org()
        AND EXISTS (
            SELECT 1 FROM contexts c
            JOIN team_members m ON m.team_id = c.team_id
            WHERE c.id = context_links.source_id
              AND m.user_id = nanace_current_user() AND m.role IN ('writer', 'admin')
        )
    );
