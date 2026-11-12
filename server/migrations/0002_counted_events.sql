-- Schema v2: one counted row per event_key across devices (D029).
-- Re-pairing a computer creates a new device that re-uploads the same
-- events; the dashboard counts each event_key once, on an active device
-- before a revoked one, then on the oldest device. Recreate the view when
-- usage_events gains columns (`e.*` is expanded at creation time).

CREATE INDEX idx_events_key ON usage_events(event_key, device_id);

CREATE VIEW counted_events AS
SELECT e.* FROM usage_events e
WHERE NOT EXISTS (
  SELECT 1
  FROM usage_events o
  JOIN devices od ON od.id = o.device_id
  JOIN devices ed ON ed.id = e.device_id
  WHERE o.event_key = e.event_key
    AND o.device_id <> e.device_id
    AND (od.revoked_at IS NOT NULL, od.created_at, od.id)
      < (ed.revoked_at IS NOT NULL, ed.created_at, ed.id)
);
