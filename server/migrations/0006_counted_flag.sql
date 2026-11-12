-- Schema v6: the cross-device dedup of D029 is stored per event (D047).
-- Until now `counted_events` decided on every read, for every row, whether
-- another device held the same event_key; a dashboard summary did that
-- several times per request. The rule itself is unchanged: among the rows
-- of one tenant sharing an event_key, the one from the device that sorts
-- first by (revoked, created_at, id) is counted. `counted` now holds that
-- answer and the triggers below keep it true whenever it can change:
--   * an event arrives whose key another device already holds;
--   * an event is deleted;
--   * a device is revoked (or its ordering columns change).

DROP VIEW counted_events;

ALTER TABLE usage_events ADD COLUMN counted INTEGER NOT NULL DEFAULT 1;

UPDATE usage_events SET counted = 0
WHERE EXISTS (
  SELECT 1
  FROM usage_events o
  JOIN devices od ON od.id = o.device_id
  JOIN devices ed ON ed.id = usage_events.device_id
  WHERE o.event_key = usage_events.event_key
    AND o.tenant_id = usage_events.tenant_id
    AND o.device_id <> usage_events.device_id
    AND (od.revoked_at IS NOT NULL, od.created_at, od.id)
      < (ed.revoked_at IS NOT NULL, ed.created_at, ed.id)
);

CREATE VIEW counted_events AS
SELECT * FROM usage_events WHERE counted = 1;

CREATE TRIGGER usage_events_counted_insert AFTER INSERT ON usage_events
WHEN EXISTS (
  SELECT 1 FROM usage_events o
  WHERE o.tenant_id = NEW.tenant_id AND o.event_key = NEW.event_key
    AND o.device_id <> NEW.device_id
)
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
  WHERE tenant_id = NEW.tenant_id AND event_key = NEW.event_key;
END;

CREATE TRIGGER usage_events_counted_delete AFTER DELETE ON usage_events
WHEN OLD.counted = 1
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
  WHERE tenant_id = OLD.tenant_id AND event_key = OLD.event_key;
END;

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
    AND event_key IN (SELECT d.event_key FROM usage_events d WHERE d.device_id = NEW.id);
END;
