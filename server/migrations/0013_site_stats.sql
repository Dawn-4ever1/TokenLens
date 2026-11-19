-- Schema v13: aggregate counts for the public website. One row per day and
-- metric; no visitor identifier, IP address, user agent or cookie is stored.
CREATE TABLE site_daily_counts (
  day TEXT NOT NULL,
  metric TEXT NOT NULL,
  count INTEGER NOT NULL DEFAULT 0 CHECK (count >= 0),
  PRIMARY KEY (day, metric)
) STRICT;
