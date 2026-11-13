-- Schema v7: account login and a replaceable, persistent tenant pairing code.
-- Device ingest/read credentials remain separate from account sessions.

CREATE TABLE accounts (
  id TEXT PRIMARY KEY,
  email TEXT NOT NULL UNIQUE,
  tenant_id TEXT NOT NULL REFERENCES tenants(id),
  created_at INTEGER NOT NULL
) STRICT;
CREATE INDEX idx_accounts_tenant ON accounts(tenant_id);

CREATE TABLE account_sessions (
  token_hash TEXT PRIMARY KEY,
  account_id TEXT NOT NULL REFERENCES accounts(id),
  tenant_id TEXT NOT NULL REFERENCES tenants(id),
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  revoked_at INTEGER
) STRICT;
CREATE INDEX idx_account_sessions_account ON account_sessions(account_id, expires_at);
CREATE INDEX idx_account_sessions_tenant ON account_sessions(tenant_id, expires_at);

CREATE TABLE email_challenges (
  email TEXT PRIMARY KEY,
  code_hash TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0,
  sent_count INTEGER NOT NULL DEFAULT 1,
  window_started_at INTEGER NOT NULL,
  last_sent_at INTEGER NOT NULL,
  consumed_at INTEGER
) STRICT;
CREATE INDEX idx_email_challenges_expiry ON email_challenges(expires_at);

CREATE TABLE tenant_pair_secrets (
  tenant_id TEXT PRIMARY KEY REFERENCES tenants(id),
  code_hash TEXT NOT NULL,
  encrypted_code TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  rotated_at INTEGER NOT NULL
) STRICT;

CREATE TRIGGER account_sessions_tenant_insert BEFORE INSERT ON account_sessions
WHEN NEW.tenant_id IS NOT (SELECT tenant_id FROM accounts WHERE id = NEW.account_id)
  OR NOT EXISTS (SELECT 1 FROM tenants WHERE id = NEW.tenant_id AND revoked_at IS NULL)
BEGIN SELECT RAISE(ABORT, 'session tenant differs from account'); END;
CREATE TRIGGER account_sessions_tenant_fixed BEFORE UPDATE OF tenant_id, account_id ON account_sessions
WHEN NEW.tenant_id IS NOT OLD.tenant_id OR NEW.account_id IS NOT OLD.account_id
BEGIN SELECT RAISE(ABORT, 'session tenant is fixed'); END;
CREATE TRIGGER accounts_tenant_insert BEFORE INSERT ON accounts
WHEN NOT EXISTS (SELECT 1 FROM tenants WHERE id = NEW.tenant_id AND revoked_at IS NULL)
BEGIN SELECT RAISE(ABORT, 'invalid account tenant'); END;
CREATE TRIGGER accounts_tenant_fixed BEFORE UPDATE OF tenant_id ON accounts
WHEN NEW.tenant_id IS NOT OLD.tenant_id
BEGIN SELECT RAISE(ABORT, 'account tenant is fixed'); END;
