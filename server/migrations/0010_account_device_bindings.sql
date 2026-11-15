-- Schema v10: retries of one local login/pairing operation return the same
-- device. This is a random installation request id, not a hardware identifier;
-- it is only unique inside one account. Legacy requests without it work.

CREATE TABLE account_device_bindings (
  account_id TEXT NOT NULL REFERENCES accounts(id),
  client_binding_id TEXT NOT NULL,
  device_id TEXT NOT NULL UNIQUE REFERENCES devices(id),
  created_at INTEGER NOT NULL,
  PRIMARY KEY (account_id, client_binding_id)
) STRICT;
CREATE INDEX idx_account_device_bindings_device ON account_device_bindings(device_id);

CREATE TRIGGER account_device_bindings_insert BEFORE INSERT ON account_device_bindings
WHEN (SELECT tenant_id FROM accounts WHERE id = NEW.account_id) IS NOT
     (SELECT tenant_id FROM devices WHERE id = NEW.device_id)
BEGIN SELECT RAISE(ABORT, 'binding account and device tenants differ'); END;

CREATE TRIGGER account_device_bindings_fixed
BEFORE UPDATE OF account_id, client_binding_id, device_id ON account_device_bindings
BEGIN SELECT RAISE(ABORT, 'account device binding is fixed'); END;
