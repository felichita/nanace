-- Listing a team's pages by tag (for example `trace-<id>` for the summary of
-- a P2P exchange, or `verdict`) uses this index.
CREATE INDEX contexts_tags_idx ON contexts USING gin (tags);
