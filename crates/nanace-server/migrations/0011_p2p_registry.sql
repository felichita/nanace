-- Peer-to-peer registry: which libp2p peer belongs to which user, and which
-- peers are present in a team right now.
--
-- Both tables are under forced row-level security with policies for
-- `nanace_tenant` only, like `contexts`. Three scopes exist:
--   * org + user (`db::scoped_tx`): team presence;
--   * user only (`db::user_tx`): a user's own peers, and looking a peer up
--     across the caller's teams;
--   * system (`db::p2p_system_tx`): the server's own maintenance, which may
--     read both tables and delete presence that has already expired.

CREATE TABLE p2p_peers (
    peer_id         text PRIMARY KEY,
    user_id         uuid NOT NULL REFERENCES users (id),
    public_key      bytea NOT NULL CHECK (octet_length(public_key) = 32),
    client_kind     text NOT NULL CHECK (client_kind IN ('desktop', 'server')),
    client_version  text NOT NULL,
    created_at      timestamptz NOT NULL DEFAULT now(),
    last_seen_at    timestamptz,
    -- A revoked peer stays as a tombstone, so its id is never bound again.
    revoked_at      timestamptz
);
CREATE INDEX p2p_peers_user_idx ON p2p_peers (user_id);

CREATE TABLE p2p_presence (
    org_id         uuid NOT NULL,
    team_id        uuid NOT NULL,
    user_id        uuid NOT NULL,
    peer_id        text NOT NULL REFERENCES p2p_peers (peer_id) ON DELETE CASCADE,
    agent_label    text NOT NULL,
    capabilities   text[] NOT NULL DEFAULT '{}',
    multiaddrs     text[] NOT NULL DEFAULT '{}',
    accept_mode    text NOT NULL CHECK (accept_mode IN ('off', 'ask', 'trusted', 'team')),
    -- Rotation chain of the token that last announced this presence; revoking
    -- the chain removes the presence. Null for a session.
    auth_chain_id  uuid,
    started_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    expires_at     timestamptz NOT NULL,
    PRIMARY KEY (team_id, peer_id, agent_label),
    FOREIGN KEY (org_id, team_id) REFERENCES teams (org_id, id) ON DELETE CASCADE,
    -- Presence ends the moment its user leaves the team.
    FOREIGN KEY (team_id, user_id) REFERENCES team_members (team_id, user_id) ON DELETE CASCADE
);
CREATE INDEX p2p_presence_peer_idx ON p2p_presence (peer_id);
CREATE INDEX p2p_presence_user_chain_idx ON p2p_presence (user_id, auth_chain_id);
CREATE INDEX p2p_presence_expires_idx ON p2p_presence (expires_at);

GRANT SELECT, INSERT, UPDATE, DELETE ON p2p_peers, p2p_presence TO nanace_tenant;

CREATE FUNCTION nanace_p2p_system() RETURNS boolean
    LANGUAGE sql STABLE
    AS $$ SELECT COALESCE(current_setting('nanace.p2p_system', true), '') = 'on' $$;

ALTER TABLE p2p_peers ENABLE ROW LEVEL SECURITY;
ALTER TABLE p2p_peers FORCE ROW LEVEL SECURITY;
ALTER TABLE p2p_presence ENABLE ROW LEVEL SECURITY;
ALTER TABLE p2p_presence FORCE ROW LEVEL SECURITY;

-- A peer is visible to its owner and to users who share a team with the owner.
CREATE POLICY p2p_peers_select ON p2p_peers FOR SELECT TO nanace_tenant
    USING (
        user_id = nanace_current_user()
        OR nanace_p2p_system()
        OR EXISTS (
            SELECT 1 FROM team_members mine
            JOIN team_members theirs ON theirs.team_id = mine.team_id
            WHERE mine.user_id = nanace_current_user() AND theirs.user_id = p2p_peers.user_id
        )
    );

CREATE POLICY p2p_peers_insert ON p2p_peers FOR INSERT TO nanace_tenant
    WITH CHECK (user_id = nanace_current_user() AND revoked_at IS NULL);

CREATE POLICY p2p_peers_update ON p2p_peers FOR UPDATE TO nanace_tenant
    USING (user_id = nanace_current_user())
    WITH CHECK (user_id = nanace_current_user());

-- Only account deletion removes peers; revoking keeps the row.
CREATE POLICY p2p_peers_delete ON p2p_peers FOR DELETE TO nanace_tenant
    USING (user_id = nanace_current_user());

-- A peer's id, owner and key never change, and a revocation is final.
CREATE FUNCTION nanace_p2p_peers_identity_unchanged() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.peer_id <> OLD.peer_id OR NEW.user_id <> OLD.user_id OR NEW.public_key <> OLD.public_key
       OR NEW.created_at <> OLD.created_at THEN
        RAISE EXCEPTION 'a peer''s id, owner, key and creation time cannot change';
    END IF;
    IF OLD.revoked_at IS NOT NULL AND NEW.revoked_at IS DISTINCT FROM OLD.revoked_at THEN
        RAISE EXCEPTION 'a revoked peer cannot be restored';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER p2p_peers_identity_unchanged BEFORE UPDATE ON p2p_peers
    FOR EACH ROW EXECUTE FUNCTION nanace_p2p_peers_identity_unchanged();

-- Presence is visible to members of its team. With an org in scope the row
-- must also belong to that org; the user-only scope spans the caller's orgs.
CREATE POLICY p2p_presence_select ON p2p_presence FOR SELECT TO nanace_tenant
    USING (
        nanace_p2p_system()
        OR (
            (nanace_current_org() IS NULL OR org_id = nanace_current_org())
            AND EXISTS (
                SELECT 1 FROM team_members m
                WHERE m.team_id = p2p_presence.team_id AND m.user_id = nanace_current_user()
            )
        )
    );

-- A user announces only their own, unrevoked peers. Team membership is
-- enforced by the foreign key to team_members.
CREATE POLICY p2p_presence_insert ON p2p_presence FOR INSERT TO nanace_tenant
    WITH CHECK (
        org_id = nanace_current_org()
        AND user_id = nanace_current_user()
        AND EXISTS (
            SELECT 1 FROM p2p_peers p
            WHERE p.peer_id = p2p_presence.peer_id AND p.user_id = nanace_current_user()
              AND p.revoked_at IS NULL
        )
    );

CREATE POLICY p2p_presence_update ON p2p_presence FOR UPDATE TO nanace_tenant
    USING (org_id = nanace_current_org() AND user_id = nanace_current_user())
    WITH CHECK (
        org_id = nanace_current_org()
        AND user_id = nanace_current_user()
        AND EXISTS (
            SELECT 1 FROM p2p_peers p
            WHERE p.peer_id = p2p_presence.peer_id AND p.user_id = nanace_current_user()
              AND p.revoked_at IS NULL
        )
    );

CREATE POLICY p2p_presence_delete ON p2p_presence FOR DELETE TO nanace_tenant
    USING (
        user_id = nanace_current_user()
        OR (nanace_p2p_system() AND expires_at <= now())
    );
