-- Schema v1: architecture section 7 plus pairing codes and heartbeat columns.
-- All times are UTC milliseconds.

CREATE TABLE devices (
  id TEXT PRIMARY KEY,
  display_name TEXT,
  platform TEXT, os_name TEXT, os_version TEXT,
  form_factor TEXT CHECK (form_factor IN ('laptop', 'desktop')),
  created_at INTEGER NOT NULL,
  last_seen_at INTEGER, last_upload_at INTEGER,
  collector_version TEXT, tokscale_rev TEXT, parser_generation TEXT,
  backfill_complete INTEGER NOT NULL DEFAULT 0,
  last_scan_at INTEGER, last_scan_duration_ms INTEGER, outbox_backlog INTEGER,
  revoked_at INTEGER
);

CREATE TABLE device_tokens (
  id TEXT PRIMARY KEY,
  device_id TEXT NOT NULL REFERENCES devices(id),
  kind TEXT NOT NULL CHECK (kind IN ('ingest', 'read')),
  token_hash TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL,
  last_used_at INTEGER,
  revoked_at INTEGER
);
CREATE INDEX idx_device_tokens_device ON device_tokens(device_id);

CREATE TABLE usage_events (
  device_id TEXT NOT NULL REFERENCES devices(id),
  event_key TEXT NOT NULL,
  client TEXT NOT NULL,
  session_id TEXT NOT NULL,
  parent_session_id TEXT,
  model_id TEXT NOT NULL,
  provider_id TEXT,
  ts INTEGER NOT NULL,
  input INTEGER NOT NULL, output INTEGER NOT NULL,
  cache_read INTEGER NOT NULL, cache_write INTEGER NOT NULL,
  cache_write_1h INTEGER NOT NULL DEFAULT 0, reasoning INTEGER NOT NULL DEFAULT 0,
  cost_usd REAL, cost_source TEXT NOT NULL,
  precision TEXT NOT NULL CHECK (precision IN ('measured', 'estimated', 'unknown')),
  credits REAL, rate_version TEXT,
  project_hash TEXT, project_label TEXT,
  message_count INTEGER NOT NULL DEFAULT 1,
  is_turn_start INTEGER NOT NULL DEFAULT 0,
  duration_ms INTEGER,
  collector_version TEXT NOT NULL, tokscale_rev TEXT NOT NULL,
  scan_generation TEXT NOT NULL,
  ingested_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
  PRIMARY KEY (device_id, event_key)
);
CREATE INDEX idx_events_ts ON usage_events(ts);
CREATE INDEX idx_events_client_ts ON usage_events(client, ts);
CREATE INDEX idx_events_session ON usage_events(device_id, client, session_id, ts);
CREATE INDEX idx_events_model_ts ON usage_events(model_id, ts);

CREATE TABLE device_client_status (
  device_id TEXT NOT NULL REFERENCES devices(id),
  client TEXT NOT NULL,
  detected INTEGER NOT NULL,
  last_event_at INTEGER,
  last_scan_events INTEGER,
  error_code TEXT,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (device_id, client)
);

CREATE TABLE ingest_batches (
  batch_id TEXT PRIMARY KEY,
  device_id TEXT NOT NULL REFERENCES devices(id),
  received_at INTEGER NOT NULL,
  accepted INTEGER, updated INTEGER, duplicates INTEGER, rejected INTEGER
);

-- Only the sha256 of each code is stored. created_by_device_id is NULL for
-- codes made with the server CLI (the first device).
CREATE TABLE pair_codes (
  code_hash TEXT PRIMARY KEY,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  used_at INTEGER,
  revoked_at INTEGER,
  created_by_device_id TEXT REFERENCES devices(id),
  claimed_device_id TEXT REFERENCES devices(id)
);
