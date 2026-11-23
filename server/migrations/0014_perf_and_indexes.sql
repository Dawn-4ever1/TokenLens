-- Schema v14: query/trigger efficiency and covering partial indexes.
-- Removes un-scoped legacy indexes superseded by tenant-scoped partial indexes,
-- indexes tenant_pair_secrets lookups by code_hash, and scopes device order
-- recounting to event_keys shared with at least one other device in the tenant.

DROP INDEX IF EXISTS idx_events_ts;
DROP INDEX IF EXISTS idx_events_client_ts;
DROP INDEX IF EXISTS idx_events_model_ts;
DROP INDEX IF EXISTS idx_events_key;

CREATE INDEX idx_events_counted_tenant_ts
  ON usage_events(tenant_id, ts)
  WHERE counted = 1;

CREATE INDEX idx_events_counted_session_parent
  ON usage_events(tenant_id, parent_session_id, ts)
  WHERE counted = 1 AND parent_session_id IS NOT NULL;

CREATE INDEX idx_events_counted_session_title
  ON usage_events(tenant_id, client, session_id, ts DESC)
  WHERE counted = 1 AND session_title IS NOT NULL;

CREATE INDEX idx_tenant_pair_secrets_hash
  ON tenant_pair_secrets(code_hash);

DROP TRIGGER IF EXISTS devices_counted_order;
CREATE TRIGGER devices_counted_order AFTER UPDATE OF revoked_at, created_at ON devices
WHEN (NEW.revoked_at IS NULL) IS NOT (OLD.revoked_at IS NULL)
  OR NEW.created_at IS NOT OLD.created_at
BEGIN
  UPDATE usage_events SET counted = NOT EXISTS (
    SELECT 1
    FROM usage_events o
    JOIN devices od ON od.id = o.device_id
    JOIN devices ed ON ed.id = usage_events.device_id
    WHERE o.event_key = usage_events.event_key
      AND o.tenant_id = usage_events.tenant_id
      AND o.device_id <> usage_events.device_id
      AND (od.revoked_at IS NOT NULL, od.created_at, od.id)
        < (ed.revoked_at IS NOT NULL, ed.created_at, ed.id)
  )
  WHERE tenant_id = NEW.tenant_id
    AND event_key IN (
      SELECT d.event_key
      FROM usage_events d
      WHERE d.tenant_id = NEW.tenant_id
        AND d.device_id = NEW.id
        AND EXISTS (
          SELECT 1
          FROM usage_events o
          WHERE o.tenant_id = NEW.tenant_id
            AND o.event_key = d.event_key
            AND o.device_id <> NEW.id
        )
    );
END;
