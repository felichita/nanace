-- The parsed P2P policy of a team, kept in step with its `Policy: P2P` page
-- in the same transaction as every write to that page. One row per team, so
-- a team cannot have two policy pages. No row, or `valid = false`, means P2P
-- is off for the team.
CREATE TABLE p2p_policies (
    team_id     uuid PRIMARY KEY,
    org_id      uuid NOT NULL,
    context_id  uuid NOT NULL UNIQUE,
    -- sha256 of the page content the row was parsed from.
    version     text NOT NULL,
    -- Null when the page did not parse.
    parsed      jsonb,
    valid       boolean NOT NULL,
    errors      jsonb NOT NULL DEFAULT '[]',
    updated_by  uuid NOT NULL REFERENCES users (id),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (org_id, team_id) REFERENCES teams (org_id, id) ON DELETE CASCADE,
    CHECK (valid = (parsed IS NOT NULL))
);

GRANT SELECT, INSERT, UPDATE, DELETE ON p2p_policies TO nanace_tenant;

ALTER TABLE p2p_policies ENABLE ROW LEVEL SECURITY;
ALTER TABLE p2p_policies FORCE ROW LEVEL SECURITY;

-- Every member reads the team's policy; like presence, the user-only scope
-- spans the caller's organizations.
CREATE POLICY p2p_policies_select ON p2p_policies FOR SELECT TO nanace_tenant
    USING (
        (nanace_current_org() IS NULL OR org_id = nanace_current_org())
        AND nanace_team_role(team_id) IS NOT NULL
    );

-- Only a team admin changes it, as with the page it comes from.
CREATE POLICY p2p_policies_insert ON p2p_policies FOR INSERT TO nanace_tenant
    WITH CHECK (
        org_id = nanace_current_org() AND updated_by = nanace_current_user()
        AND nanace_team_role(team_id) = 'admin'
    );

CREATE POLICY p2p_policies_update ON p2p_policies FOR UPDATE TO nanace_tenant
    USING (org_id = nanace_current_org() AND nanace_team_role(team_id) = 'admin')
    WITH CHECK (
        org_id = nanace_current_org() AND updated_by = nanace_current_user()
        AND nanace_team_role(team_id) = 'admin'
    );

CREATE POLICY p2p_policies_delete ON p2p_policies FOR DELETE TO nanace_tenant
    USING (org_id = nanace_current_org() AND nanace_team_role(team_id) = 'admin');
