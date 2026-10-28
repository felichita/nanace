-- Rotation chains: a rotated token gets the chain id of the token it
-- replaced, and revoking any token of a chain revokes all of them. A leaked
-- token that was rotated by whoever holds it therefore dies with the
-- original. Existing tokens each start their own chain.
ALTER TABLE api_tokens ADD COLUMN chain_id uuid;
UPDATE api_tokens SET chain_id = id;
ALTER TABLE api_tokens ALTER COLUMN chain_id SET NOT NULL;
CREATE INDEX api_tokens_chain_idx ON api_tokens (chain_id);
