#!/bin/sh
# Daily backup: copies the live database into ./backups and keeps KEEP_DAYS
# days (default 30). Run from cron on the server, for example:
#   15 4 * * * /opt/tokenlens/deploy/backup.sh >> /opt/tokenlens/deploy/backups/backup.log 2>&1
set -eu
cd "$(dirname "$0")"

keep_days="${KEEP_DAYS:-30}"
name="tokenlens-$(date -u +%Y%m%d-%H%M%S).sqlite"

# The server keeps running; the copy is integrity-checked before it is kept.
docker compose exec -T dashboard tokenlens-server backup "/app/backups/$name"

# Only files this script made, and only after a fresh copy succeeded above.
find backups -maxdepth 1 -type f -name 'tokenlens-*.sqlite' -mtime +"$keep_days" -print -delete
