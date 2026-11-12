-- Schema v3: a random id for this copy of the database (D031). A restored
-- backup gets a new id; a computer that sees the id change re-sends its
-- history, and the server counts what it already has as duplicates.

CREATE TABLE server_meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
) STRICT;

INSERT INTO server_meta (key, value) VALUES ('store_id', lower(hex(randomblob(16))));
