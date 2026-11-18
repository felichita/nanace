-- Journal of P2P exchanges: who sent what kind of message to whom, and how
-- it ended. Metadata only; message bodies never reach the server. Each side
-- of an exchange reports it, with the sender's signed header, so a row is
-- not just the reporter's word.
CREATE TABLE p2p_exchanges (
    id               uuid PRIMARY KEY,
    org_id           uuid NOT NULL,
    team_id          uuid NOT NULL,
    request_id       uuid NOT NULL,
    reported_by      text NOT NULL CHECK (reported_by IN ('sender', 'receiver')),
    reporter_user    uuid NOT NULL REFERENCES users (id),
    trace_id         text NOT NULL,
    from_peer        text NOT NULL,
    from_agent       text NOT NULL,
    from_user        uuid NOT NULL REFERENCES users (id),
    to_peer          text NOT NULL,
    to_agent         text NOT NULL,
    to_user          uuid NOT NULL REFERENCES users (id),
    kind             text NOT NULL CHECK (kind IN ('ask', 'status', 'notify')),
    status           text NOT NULL CHECK (status IN ('delivered', 'answered', 'rejected', 'expired', 'failed')),
    reject_reason    text,
    consent          text CHECK (consent IN ('auto', 'user_approved', 'user_denied')),
    hop              integer NOT NULL,
    -- Body sizes of the request and of the answer.
    bytes_out        integer NOT NULL,
    bytes_in         integer NOT NULL,
    -- sha256 of the signed request header.
    request_hash     text NOT NULL,
    -- True when the report carried the receiver's signed answer header.
    answer_verified  boolean NOT NULL,
    policy_version   text NOT NULL,
    sent_at          timestamptz NOT NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    UNIQUE (request_id, reported_by),
    FOREIGN KEY (org_id, team_id) REFERENCES teams (org_id, id) ON DELETE CASCADE
);
CREATE INDEX p2p_exchanges_team_created_idx ON p2p_exchanges (team_id, created_at DESC);
CREATE INDEX p2p_exchanges_created_idx ON p2p_exchanges (created_at);

GRANT SELECT, INSERT, UPDATE, DELETE ON p2p_exchanges TO nanace_tenant;

ALTER TABLE p2p_exchanges ENABLE ROW LEVEL SECURITY;
ALTER TABLE p2p_exchanges FORCE ROW LEVEL SECURITY;

-- The journal is read by the team's admins and the organization's admins.
-- It holds no page content, so org admins reading it does not break "no
-- implicit access to team content". A reporter also sees its own rows.
CREATE POLICY p2p_exchanges_select ON p2p_exchanges FOR SELECT TO nanace_tenant
    USING (
        nanace_p2p_system()
        OR (
            org_id = nanace_current_org()
            AND (
                reporter_user = nanace_current_user()
                OR nanace_team_role(team_id) = 'admin'
                OR EXISTS (
                    SELECT 1 FROM org_members o
                    WHERE o.org_id = p2p_exchanges.org_id AND o.user_id = nanace_current_user()
                      AND o.role = 'admin'
                )
            )
        )
    );

-- A member reports only its own side of an exchange.
CREATE POLICY p2p_exchanges_insert ON p2p_exchanges FOR INSERT TO nanace_tenant
    WITH CHECK (
        org_id = nanace_current_org()
        AND reporter_user = nanace_current_user()
        AND nanace_team_role(team_id) IS NOT NULL
        AND (
            (reported_by = 'sender' AND from_user = nanace_current_user())
            OR (reported_by = 'receiver' AND to_user = nanace_current_user())
        )
    );

CREATE POLICY p2p_exchanges_update ON p2p_exchanges FOR UPDATE TO nanace_tenant
    USING (org_id = nanace_current_org() AND reporter_user = nanace_current_user())
    WITH CHECK (org_id = nanace_current_org() AND reporter_user = nanace_current_user());

-- Only retention deletes.
CREATE POLICY p2p_exchanges_delete ON p2p_exchanges FOR DELETE TO nanace_tenant
    USING (nanace_p2p_system());

-- A report cannot be turned into a different exchange.
CREATE FUNCTION nanace_p2p_exchanges_identity_unchanged() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.id <> OLD.id OR NEW.org_id <> OLD.org_id OR NEW.team_id <> OLD.team_id
       OR NEW.request_id <> OLD.request_id OR NEW.reported_by <> OLD.reported_by
       OR NEW.reporter_user <> OLD.reporter_user OR NEW.request_hash <> OLD.request_hash
       OR NEW.from_user <> OLD.from_user OR NEW.to_user <> OLD.to_user
       OR NEW.from_peer <> OLD.from_peer OR NEW.to_peer <> OLD.to_peer
       OR NEW.kind <> OLD.kind OR NEW.created_at <> OLD.created_at THEN
        RAISE EXCEPTION 'an exchange report cannot change what it reports';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER p2p_exchanges_identity_unchanged BEFORE UPDATE ON p2p_exchanges
    FOR EACH ROW EXECUTE FUNCTION nanace_p2p_exchanges_identity_unchanged();

-- The server's maintenance scope also reads policies: the network node's
-- allow list is "peers present in a team whose policy enables P2P", and
-- retention honours a team's shorter period.
DROP POLICY p2p_policies_select ON p2p_policies;
CREATE POLICY p2p_policies_select ON p2p_policies FOR SELECT TO nanace_tenant
    USING (
        nanace_p2p_system()
        OR (
            (nanace_current_org() IS NULL OR org_id = nanace_current_org())
            AND nanace_team_role(team_id) IS NOT NULL
        )
    );
