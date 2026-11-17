-- Schema v12: retain source-provided session titles for the Sessions view.
-- Project metadata already had columns; new collectors now populate them from
-- the upstream workspace label and a one-way workspace hash.

ALTER TABLE usage_events ADD COLUMN session_title TEXT;
CREATE INDEX idx_events_session_title ON usage_events(tenant_id, session_title);
