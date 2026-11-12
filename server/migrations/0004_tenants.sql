-- Schema v4: tenant/workspace isolation.
-- Existing data belongs to one personal tenant.  New devices inherit the
-- tenant carried by their pairing code; clients never choose tenant_id.

CREATE TABLE tenants (
  id TEXT PRIMARY KEY,
  slug TEXT NOT NULL UNIQUE,
  display_name TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  revoked_at INTEGER
);

INSERT INTO tenants (id, slug, display_name, created_at)
VALUES ('tenant_personal', 'personal', 'Personal', 0);

DROP VIEW counted_events;

ALTER TABLE devices
  ADD COLUMN tenant_id TEXT REFERENCES tenants(id);
ALTER TABLE device_tokens
  ADD COLUMN tenant_id TEXT REFERENCES tenants(id);
ALTER TABLE usage_events
  ADD COLUMN tenant_id TEXT REFERENCES tenants(id);
ALTER TABLE device_client_status
  ADD COLUMN tenant_id TEXT REFERENCES tenants(id);
ALTER TABLE pair_codes
  ADD COLUMN tenant_id TEXT REFERENCES tenants(id);

UPDATE devices SET tenant_id = 'tenant_personal' WHERE tenant_id IS NULL;
UPDATE device_tokens SET tenant_id = (
  SELECT tenant_id FROM devices WHERE devices.id = device_tokens.device_id
) WHERE tenant_id IS NULL;
UPDATE usage_events SET tenant_id = (
  SELECT tenant_id FROM devices WHERE devices.id = usage_events.device_id
) WHERE tenant_id IS NULL;
UPDATE device_client_status SET tenant_id = (
  SELECT tenant_id FROM devices WHERE devices.id = device_client_status.device_id
) WHERE tenant_id IS NULL;
UPDATE pair_codes SET tenant_id = COALESCE(
  (SELECT tenant_id FROM devices WHERE devices.id = pair_codes.created_by_device_id),
  'tenant_personal'
) WHERE tenant_id IS NULL;

-- ingest_batches used to have a global batch_id primary key.  A batch id is
-- only unique for one device in one tenant, so make that scope explicit.
CREATE TABLE ingest_batches_new (
  tenant_id TEXT NOT NULL REFERENCES tenants(id),
  batch_id TEXT NOT NULL,
  device_id TEXT NOT NULL REFERENCES devices(id),
  received_at INTEGER NOT NULL,
  accepted INTEGER, updated INTEGER, duplicates INTEGER, rejected INTEGER,
  PRIMARY KEY (tenant_id, device_id, batch_id)
);
INSERT INTO ingest_batches_new
  (tenant_id, batch_id, device_id, received_at, accepted, updated, duplicates, rejected)
SELECT COALESCE(d.tenant_id, 'tenant_personal'), b.batch_id, b.device_id,
       b.received_at, b.accepted, b.updated, b.duplicates, b.rejected
FROM ingest_batches b
LEFT JOIN devices d ON d.id = b.device_id;
DROP TABLE ingest_batches;
ALTER TABLE ingest_batches_new RENAME TO ingest_batches;

CREATE INDEX idx_devices_tenant ON devices(tenant_id, created_at, id);
CREATE INDEX idx_device_tokens_tenant ON device_tokens(tenant_id, device_id);
CREATE INDEX idx_events_tenant_ts ON usage_events(tenant_id, ts);
CREATE INDEX idx_events_tenant_key ON usage_events(tenant_id, event_key, device_id);
CREATE INDEX idx_status_tenant ON device_client_status(tenant_id, device_id);
CREATE INDEX idx_batches_tenant_received ON ingest_batches(tenant_id, received_at);
CREATE INDEX idx_pair_codes_tenant ON pair_codes(tenant_id, created_at);

-- SQLite cannot add a NOT NULL REFERENCES column to an existing table, so
-- these triggers carry the invariants for every write after the backfill:
--   * every row names a tenant, and a row tied to a device is in that
--     device's tenant (an event can never land in another tenant's view);
--   * a tenant id, and the device a row belongs to, never change: moving a
--     device would also move history its old tenant has already seen.
CREATE TRIGGER devices_tenant_insert BEFORE INSERT ON devices
WHEN NEW.tenant_id IS NULL OR NOT EXISTS (SELECT 1 FROM tenants WHERE id = NEW.tenant_id)
BEGIN SELECT RAISE(ABORT, 'invalid device tenant'); END;
CREATE TRIGGER devices_tenant_fixed BEFORE UPDATE OF tenant_id ON devices
WHEN NEW.tenant_id IS NOT OLD.tenant_id
BEGIN SELECT RAISE(ABORT, 'device tenant is fixed'); END;

CREATE TRIGGER device_tokens_tenant_insert BEFORE INSERT ON device_tokens
WHEN NEW.tenant_id IS NULL
  OR NEW.tenant_id IS NOT (SELECT tenant_id FROM devices WHERE id = NEW.device_id)
BEGIN SELECT RAISE(ABORT, 'token tenant differs from its device'); END;
CREATE TRIGGER device_tokens_tenant_fixed BEFORE UPDATE OF tenant_id, device_id ON device_tokens
WHEN NEW.tenant_id IS NOT OLD.tenant_id OR NEW.device_id IS NOT OLD.device_id
BEGIN SELECT RAISE(ABORT, 'token tenant is fixed'); END;

CREATE TRIGGER usage_events_tenant_insert BEFORE INSERT ON usage_events
WHEN NEW.tenant_id IS NULL
  OR NEW.tenant_id IS NOT (SELECT tenant_id FROM devices WHERE id = NEW.device_id)
BEGIN SELECT RAISE(ABORT, 'event tenant differs from its device'); END;
CREATE TRIGGER usage_events_tenant_fixed BEFORE UPDATE OF tenant_id, device_id ON usage_events
WHEN NEW.tenant_id IS NOT OLD.tenant_id OR NEW.device_id IS NOT OLD.device_id
BEGIN SELECT RAISE(ABORT, 'event tenant is fixed'); END;

CREATE TRIGGER device_client_status_tenant_insert BEFORE INSERT ON device_client_status
WHEN NEW.tenant_id IS NULL
  OR NEW.tenant_id IS NOT (SELECT tenant_id FROM devices WHERE id = NEW.device_id)
BEGIN SELECT RAISE(ABORT, 'status tenant differs from its device'); END;
CREATE TRIGGER device_client_status_tenant_fixed
BEFORE UPDATE OF tenant_id, device_id ON device_client_status
WHEN NEW.tenant_id IS NOT OLD.tenant_id OR NEW.device_id IS NOT OLD.device_id
BEGIN SELECT RAISE(ABORT, 'status tenant is fixed'); END;

CREATE TRIGGER ingest_batches_tenant_insert BEFORE INSERT ON ingest_batches
WHEN NEW.tenant_id IS NULL
  OR NEW.tenant_id IS NOT (SELECT tenant_id FROM devices WHERE id = NEW.device_id)
BEGIN SELECT RAISE(ABORT, 'batch tenant differs from its device'); END;
CREATE TRIGGER ingest_batches_tenant_fixed BEFORE UPDATE OF tenant_id, device_id ON ingest_batches
WHEN NEW.tenant_id IS NOT OLD.tenant_id OR NEW.device_id IS NOT OLD.device_id
BEGIN SELECT RAISE(ABORT, 'batch tenant is fixed'); END;

-- A code made by a device stays in that device's tenant, and the device that
-- claims it joins the same tenant.
CREATE TRIGGER pair_codes_tenant_insert BEFORE INSERT ON pair_codes
WHEN NEW.tenant_id IS NULL
  OR NOT EXISTS (SELECT 1 FROM tenants WHERE id = NEW.tenant_id)
  OR (NEW.created_by_device_id IS NOT NULL AND NEW.tenant_id IS NOT
      (SELECT tenant_id FROM devices WHERE id = NEW.created_by_device_id))
  OR (NEW.claimed_device_id IS NOT NULL AND NEW.tenant_id IS NOT
      (SELECT tenant_id FROM devices WHERE id = NEW.claimed_device_id))
BEGIN SELECT RAISE(ABORT, 'invalid pairing tenant'); END;
CREATE TRIGGER pair_codes_tenant_fixed
BEFORE UPDATE OF tenant_id, created_by_device_id, claimed_device_id ON pair_codes
WHEN NEW.tenant_id IS NOT OLD.tenant_id
  OR NEW.created_by_device_id IS NOT OLD.created_by_device_id
  OR (NEW.claimed_device_id IS NOT NULL AND NEW.tenant_id IS NOT
      (SELECT tenant_id FROM devices WHERE id = NEW.claimed_device_id))
BEGIN SELECT RAISE(ABORT, 'pairing code tenant differs from its device'); END;

CREATE VIEW counted_events AS
SELECT e.* FROM usage_events e
WHERE NOT EXISTS (
  SELECT 1
  FROM usage_events o
  JOIN devices od ON od.id = o.device_id
  JOIN devices ed ON ed.id = e.device_id
  WHERE o.event_key = e.event_key
    AND o.tenant_id = e.tenant_id
    AND o.device_id <> e.device_id
    AND (od.revoked_at IS NOT NULL, od.created_at, od.id)
      < (ed.revoked_at IS NOT NULL, ed.created_at, ed.id)
);
