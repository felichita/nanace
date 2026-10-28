-- Account deletion (GDPR erasure). A deleted user's row stays because pages
-- and audit entries reference it, but it is anonymized and marked here;
-- sessions and tokens of a deleted user stop working at once.
ALTER TABLE users ADD COLUMN deleted_at timestamptz;
