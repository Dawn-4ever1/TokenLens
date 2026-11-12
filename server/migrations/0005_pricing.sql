-- Schema v5: API-equivalent cost estimated on the server (D042).
-- The three pricing-context columns are event values sent by the collector;
-- est_cost_usd, price_status and price_version are derived here from the
-- current price catalogs and are rewritten whenever the catalogs change.
--   price_status: reported | priced | partial | free | unpriced | credits,
--   NULL while no price catalog has been loaded yet.

DROP VIEW counted_events;

ALTER TABLE usage_events ADD COLUMN pricing_model_id TEXT;
ALTER TABLE usage_events ADD COLUMN billing_provider TEXT;
ALTER TABLE usage_events ADD COLUMN service_tier TEXT;
ALTER TABLE usage_events ADD COLUMN est_cost_usd REAL;
ALTER TABLE usage_events ADD COLUMN price_status TEXT;
ALTER TABLE usage_events ADD COLUMN price_version TEXT;

CREATE INDEX idx_events_tenant_price ON usage_events(tenant_id, price_version);

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
