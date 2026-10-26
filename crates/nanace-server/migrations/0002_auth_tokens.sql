-- Personal access tokens and one-time login codes.
-- Both store only a SHA-256 hash of a high-entropy random secret.

CREATE TABLE api_tokens (
    id            uuid PRIMARY KEY,
    user_id       uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name          text NOT NULL,
    token_hash    text NOT NULL UNIQUE,
    -- First characters of the token, shown so users can tell tokens apart.
    token_prefix  text NOT NULL,
    scope         text NOT NULL CHECK (scope IN ('read', 'write')),
    created_at    timestamptz NOT NULL DEFAULT now(),
    expires_at    timestamptz NOT NULL,
    last_used_at  timestamptz,
    revoked_at    timestamptz
);
CREATE INDEX api_tokens_user_idx ON api_tokens (user_id, created_at DESC);

-- Handed to the browser after Google sign-in and exchanged once for a
-- session token, so the session token never appears in a URL.
CREATE TABLE login_codes (
    code_hash   text PRIMARY KEY,
    user_id     uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    expires_at  timestamptz NOT NULL
);
CREATE INDEX login_codes_expires_idx ON login_codes (expires_at);
