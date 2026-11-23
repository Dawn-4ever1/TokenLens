#!/bin/sh
# Daily backup: copies the live database into ./backups and keeps KEEP_DAYS
# days (default 30). Run from cron on the server, for example:
#   15 4 * * * /opt/tokenlens/deploy/backup.sh >> /opt/tokenlens/deploy/backups/backup.log 2>&1
set -eu
umask 077
cd "$(dirname "$0")"

keep_days="${KEEP_DAYS:-30}"
case "$keep_days" in
  ''|*[!0-9]*)
    echo "KEEP_DAYS must be a non-negative integer, got: $keep_days" >&2
    exit 2
    ;;
esac
name="tokenlens-$(date -u +%Y%m%d-%H%M%S).sqlite"

# The server keeps running; the copy is integrity-checked before it is kept.
docker compose exec -T dashboard tokenlens-server backup "/app/backups/$name"

# Remove leftover incomplete backup sidecars from interrupted runs, then prune
# completed backups older than keep_days.
find backups/ -maxdepth 1 -type f -name 'tokenlens-*.sqlite.partial*' -mtime +0 -delete
find backups/ -maxdepth 1 -type f -name 'tokenlens-*.sqlite' -mtime +"$keep_days" -print -delete
