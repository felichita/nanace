-- Login codes issued to the desktop app's loopback redirect can only be
-- exchanged with the PKCE verifier for this challenge (RFC 8252 / RFC 7636).
ALTER TABLE login_codes ADD COLUMN code_challenge text;
