-- Defense in depth below the API (which never does any of this):
--
-- 1. A trash row must copy a live context: same id, org, team, author and
--    creation time. Otherwise a writer with direct SQL access as
--    nanace_tenant could forge a trash row naming another author and restore
--    it as a context "created" by that person.
-- 2. A context's org, team, author and creation time never change after
--    insert, so an UPDATE cannot move it to another team or re-attribute it.

DROP POLICY context_trash_insert ON context_trash;
CREATE POLICY context_trash_insert ON context_trash FOR INSERT TO nanace_tenant
    WITH CHECK (
        org_id = nanace_current_org()
        AND deleted_by = nanace_current_user()
        AND EXISTS (
            SELECT 1 FROM team_members m
            WHERE m.team_id = context_trash.team_id AND m.user_id = nanace_current_user()
              AND m.role IN ('writer', 'admin')
        )
        AND EXISTS (
            SELECT 1 FROM contexts c
            WHERE c.id = context_trash.id AND c.org_id = context_trash.org_id
              AND c.team_id = context_trash.team_id AND c.created_by = context_trash.created_by
              AND c.created_at = context_trash.created_at
        )
    );

CREATE FUNCTION nanace_contexts_identity_unchanged() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.id <> OLD.id OR NEW.org_id <> OLD.org_id OR NEW.team_id <> OLD.team_id
       OR NEW.created_by <> OLD.created_by OR NEW.created_at <> OLD.created_at THEN
        RAISE EXCEPTION 'a context''s id, org, team, author and creation time cannot change';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER contexts_identity_unchanged BEFORE UPDATE ON contexts
    FOR EACH ROW EXECUTE FUNCTION nanace_contexts_identity_unchanged();
