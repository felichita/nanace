-- Follow-ups from the security review of 0.2.0.

-- 1. The exchange journal: one row per request, side and reporter. With the
--    reporter missing from the key, a member could report a request id first
--    and make the real party's report collide with a row it may not update.
ALTER TABLE p2p_exchanges DROP CONSTRAINT p2p_exchanges_request_id_reported_by_key;
ALTER TABLE p2p_exchanges ADD CONSTRAINT p2p_exchanges_request_reporter_key
    UNIQUE (request_id, reported_by, reporter_user);

-- 2. Protected titles: more characters that render as nothing are ignored
--    when a title is recognized (variation selectors, the combining grapheme
--    joiner, Hangul fillers, the braille blank), so `Wiki<U+FE0F>: Index`
--    is the protected page it looks like. Mirrors
--    `nanace_core::validate::is_protected_title`; both run the same cases.
CREATE OR REPLACE FUNCTION nanace_is_protected_title(title text) RETURNS boolean
    LANGUAGE sql IMMUTABLE
    AS $$
        SELECT regexp_replace(
            normalize(title, NFKC),
            '[\u00AD\u034F\u0600-\u0605\u061C\u06DD\u070F\u0890-\u0891\u08E2\u115F-\u1160\u17B4-\u17B5\u180B-\u180F\u200B-\u200F\u202A-\u202E\u2060-\u2064\u2066-\u206F\u2800\u3164\uFE00-\uFE0F\uFEFF\uFFA0\uFFF9-\uFFFB\U000110BD\U000110CD\U00013430-\U0001343F\U0001BCA0-\U0001BCA3\U0001D173-\U0001D17A\U000E0000-\U000E0FFF]',
            '',
            'g'
        ) ~ '^[\u0009-\u000D \u0085\u00A0\u1680\u2000-\u200A\u2028\u2029\u202F\u205F\u3000]*([wW][iI][kK][iI]|[pP][oO][lL][iI][cC][yY])[\u0009-\u000D \u0085\u00A0\u1680\u2000-\u200A\u2028\u2029\u202F\u205F\u3000]*:'
    $$;

-- 3. Links follow the page they belong to, including its protection: the
--    links of a `Wiki:` or `Policy:` page are written only by a team admin.
--    The API already refuses such writes before it touches links.
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
              AND nanace_may_write_title(c.team_id, c.title)
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
              AND nanace_may_write_title(c.team_id, c.title)
        )
    );
