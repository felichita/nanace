-- Token scope `p2p`, for the P2P daemon: it identifies the user to the peer
-- registry and nothing else, so a token file taken from a laptop cannot read
-- or change pages.
ALTER TABLE api_tokens DROP CONSTRAINT api_tokens_scope_check;
ALTER TABLE api_tokens ADD CONSTRAINT api_tokens_scope_check CHECK (scope IN ('read', 'write', 'p2p'));
