-- Token rotation: a personal access token can replace itself.
--
-- `chain_started_at` is when the first token of a rotation chain was issued;
-- rotation never extends a chain past one year, so a leaked token cannot keep
-- itself alive forever. `rotated_at` marks a token that was already replaced:
-- it keeps working for a short grace period but cannot rotate again, so a
-- chain cannot fork.
ALTER TABLE api_tokens ADD COLUMN chain_started_at timestamptz;
UPDATE api_tokens SET chain_started_at = created_at;
ALTER TABLE api_tokens ALTER COLUMN chain_started_at SET NOT NULL;
ALTER TABLE api_tokens ALTER COLUMN chain_started_at SET DEFAULT now();
ALTER TABLE api_tokens ADD COLUMN rotated_at timestamptz;
