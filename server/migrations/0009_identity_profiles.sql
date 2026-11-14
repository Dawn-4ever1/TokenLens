-- Public GitHub profile fields for display, never identity keys. The
-- provider's immutable subject remains the only identity (D049).
ALTER TABLE account_identities ADD COLUMN display_name TEXT;
ALTER TABLE account_identities ADD COLUMN avatar_url TEXT;
