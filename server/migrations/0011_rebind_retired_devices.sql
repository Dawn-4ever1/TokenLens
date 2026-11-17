-- Schema v11: one installation request may have more than one identity over
-- time. A device retired from another computer stays retired; reconnecting
-- appends a new registration instead of editing the immutable old binding.
-- Existing devices, events, credentials and bindings are copied unchanged.

CREATE TABLE account_device_bindings_v11 (
  account_id TEXT NOT NULL REFERENCES accounts(id),
  client_binding_id TEXT NOT NULL,
  device_id TEXT NOT NULL UNIQUE REFERENCES devices(id),
  created_at INTEGER NOT NULL,
  PRIMARY KEY (account_id, client_binding_id, device_id)
) STRICT;
INSERT INTO account_device_bindings_v11
  (account_id, client_binding_id, device_id, created_at)
SELECT account_id, client_binding_id, device_id, created_at
FROM account_device_bindings;
DROP TABLE account_device_bindings;
ALTER TABLE account_device_bindings_v11 RENAME TO account_device_bindings;

CREATE INDEX idx_account_device_bindings_device ON account_device_bindings(device_id);
CREATE INDEX idx_account_device_bindings_current
  ON account_device_bindings(account_id, client_binding_id, created_at DESC);

CREATE TRIGGER account_device_bindings_insert BEFORE INSERT ON account_device_bindings
WHEN (SELECT tenant_id FROM accounts WHERE id = NEW.account_id) IS NOT
     (SELECT tenant_id FROM devices WHERE id = NEW.device_id)
BEGIN SELECT RAISE(ABORT, 'binding account and device tenants differ'); END;

CREATE TRIGGER account_device_bindings_fixed
BEFORE UPDATE OF account_id, client_binding_id, device_id ON account_device_bindings
BEGIN SELECT RAISE(ABORT, 'account device binding is fixed'); END;
