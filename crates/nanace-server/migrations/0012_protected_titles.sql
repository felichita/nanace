-- Protected pages: a context whose title starts with `Wiki:` or `Policy:` is
-- written only by an admin of its team. The API refuses such writes with a
-- clear error; these policies are the same rule below the API, so a missed
-- check in a handler cannot let a writer change a team's policy or wiki
-- service pages.

-- Mirrors `nanace_core::validate::is_protected_title`; both are tested with
-- the same cases. NFKC folds full-width and compatibility forms, invisible
-- format characters (Unicode Cf) are dropped, and only ASCII case is folded,
-- so the answer does not depend on the database locale.
CREATE FUNCTION nanace_is_protected_title(title text) RETURNS boolean
    LANGUAGE sql IMMUTABLE
    AS $$
        SELECT regexp_replace(
            normalize(title, NFKC),
            '[­؀-؅؜۝܏࢐࢑࣢᠎​-‏‪-‮⁠-⁤⁦-⁯﻿￹-￻\U000110BD\U000110CD\U00013430-\U0001343F\U0001BCA0-\U0001BCA3\U0001D173-\U0001D17A\U000E0001\U000E0020-\U000E007F]',
            '',
            'g'
        ) ~ '^[\u0009-\u000D \u0085   -     　]*([wW][iI][kK][iI]|[pP][oO][lL][iI][cC][yY])[\u0009-\u000D \u0085   -     　]*:'
    $$;

-- True when the scoped user may write a context with this title in the team.
CREATE FUNCTION nanace_may_write_title(team uuid, title text) RETURNS boolean
    LANGUAGE sql STABLE
    AS $$
        SELECT NOT nanace_is_protected_title(title)
            OR EXISTS (
                SELECT 1 FROM team_members m
                WHERE m.team_id = team AND m.user_id = nanace_current_user() AND m.role = 'admin'
            )
    $$;

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
        AND nanace_may_write_title(team_id, title)
    );

-- USING sees the title before the update and WITH CHECK the one after it, so
-- a page can be renamed neither out of nor into a protected title.
DROP POLICY contexts_update ON contexts;
CREATE POLICY contexts_update ON contexts FOR UPDATE TO nanace_tenant
    USING (
        org_id = nanace_current_org()
        AND EXISTS (
            SELECT 1 FROM team_members m
            WHERE m.team_id = contexts.team_id AND m.user_id = nanace_current_user()
              AND m.role IN ('writer', 'admin')
        )
        AND nanace_may_write_title(team_id, title)
    )
    WITH CHECK (
        org_id = nanace_current_org()
        AND updated_by = nanace_current_user()
        AND EXISTS (
            SELECT 1 FROM team_members m
            WHERE m.team_id = contexts.team_id AND m.user_id = nanace_current_user()
              AND m.role IN ('writer', 'admin')
        )
        AND nanace_may_write_title(team_id, title)
    );

DROP POLICY contexts_delete ON contexts;
CREATE POLICY contexts_delete ON contexts FOR DELETE TO nanace_tenant
    USING (
        org_id = nanace_current_org()
        AND EXISTS (
            SELECT 1 FROM team_members m
            WHERE m.team_id = contexts.team_id AND m.user_id = nanace_current_user()
              AND m.role IN ('writer', 'admin')
        )
        AND nanace_may_write_title(team_id, title)
    );

-- Trashing, restoring and purging a protected page are writes to it too.
-- Writers still see it in the trash list (the SELECT policy is unchanged).
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
        AND nanace_may_write_title(team_id, title)
    );

DROP POLICY context_trash_delete ON context_trash;
CREATE POLICY context_trash_delete ON context_trash FOR DELETE TO nanace_tenant
    USING (
        org_id = nanace_current_org()
        AND EXISTS (
            SELECT 1 FROM team_members m
            WHERE m.team_id = context_trash.team_id AND m.user_id = nanace_current_user()
              AND m.role IN ('writer', 'admin')
        )
        AND nanace_may_write_title(team_id, title)
    );
