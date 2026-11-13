-- Schema v8: link an account to a third-party identity (GitHub first, Google
-- can reuse the same table). Identity is keyed by the provider's own user id,
-- never by email alone, so a changed address cannot take over an account.

CREATE TABLE account_identities (
  provider TEXT NOT NULL,
  subject TEXT NOT NULL,
  account_id TEXT NOT NULL REFERENCES accounts(id),
  email TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  last_login_at INTEGER NOT NULL,
  PRIMARY KEY (provider, subject)
) STRICT;

CREATE INDEX idx_account_identities_account ON account_identities(account_id);

CREATE TRIGGER account_identities_account_insert BEFORE INSERT ON account_identities
WHEN NOT EXISTS (SELECT 1 FROM accounts WHERE id = NEW.account_id)
BEGIN SELECT RAISE(ABORT, 'unknown account for identity'); END;

CREATE TRIGGER account_identities_fixed
BEFORE UPDATE OF provider, subject, account_id ON account_identities
WHEN NEW.provider IS NOT OLD.provider
  OR NEW.subject IS NOT OLD.subject
  OR NEW.account_id IS NOT OLD.account_id
BEGIN SELECT RAISE(ABORT, 'identity is fixed'); END;
