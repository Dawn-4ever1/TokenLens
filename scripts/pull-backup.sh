#!/bin/sh
# Pulls the newest verified SQLite backup from a remote TokenLens server into a
# local off-host folder, checks its SHA-256 and SQLite integrity, and prunes
# local copies older than KEEP_DAYS (default 30).
#
# Usage:
#   TOKENLENS_SSH_TARGET=user@your-server.example.com \
#   TOKENLENS_SSH_KEY=~/.ssh/my_key \
#   ./scripts/pull-backup.sh [local-backup-dir]
set -eu

target="${TOKENLENS_SSH_TARGET:-}"
if [ -z "$target" ]; then
  echo "Set TOKENLENS_SSH_TARGET (for example user@example.com) before running." >&2
  exit 2
fi

dest_dir="${1:-${TOKENLENS_BACKUP_DEST_DIR:-$HOME/Documents/TokenLens-backups}}"
remote_deploy="${TOKENLENS_REMOTE_DEPLOY_DIR:-/opt/tokenlens/deploy}"
keep_days="${KEEP_DAYS:-30}"

mkdir -p "$dest_dir"
chmod 700 "$dest_dir"

ssh_opts="-o BatchMode=yes -o ConnectTimeout=15"
if [ -n "${TOKENLENS_SSH_KEY:-}" ]; then
  ssh_opts="$ssh_opts -i $TOKENLENS_SSH_KEY -o IdentitiesOnly=yes"
fi

# Refresh an online backup on the server first unless SKIP_REMOTE_BACKUP=1.
if [ "${SKIP_REMOTE_BACKUP:-0}" != "1" ]; then
  # shellcheck disable=SC2086
  ssh $ssh_opts "$target" "cd '$remote_deploy' && sudo sh ./backup.sh </dev/null"
fi

# Read the newest backup filename and its SHA-256 on the server.
# shellcheck disable=SC2086
meta=$(ssh $ssh_opts "$target" "sudo sh -c '
  set -eu
  cd \"$remote_deploy\"
  latest=\$(ls -1t backups/tokenlens-*.sqlite 2>/dev/null | head -n 1 || true)
  [ -n \"\$latest\" ] || { echo \"No remote backups found.\" >&2; exit 1; }
  sum=\$(sha256sum \"\$latest\" | awk \"{print \\\$1}\")
  printf \"%s %s\\n\" \"\$(basename \"\$latest\")\" \"\$sum\"
'")

filename=$(printf "%s" "$meta" | awk '{print $1}')
expected_sha=$(printf "%s" "$meta" | awk '{print $2}')

case "$filename" in
  tokenlens-*.sqlite) ;;
  *) echo "Unexpected backup filename: $filename" >&2; exit 1 ;;
esac

local_path="$dest_dir/$filename"
tmp_path="$local_path.part.$$"
cleanup() {
  rm -f "$tmp_path"
}
trap cleanup EXIT INT TERM

if [ -f "$local_path" ]; then
  echo "Already present locally: $local_path"
else
  # Stream over SSH with root read access so backup files remain mode 600 on server.
  # shellcheck disable=SC2086
  ssh $ssh_opts "$target" "sudo cat '$remote_deploy/backups/$filename'" > "$tmp_path"
  chmod 600 "$tmp_path"
  if command -v sha256sum >/dev/null 2>&1; then
    actual_sha=$(sha256sum "$tmp_path" | awk '{print $1}')
  else
    actual_sha=$(shasum -a 256 "$tmp_path" | awk '{print $1}')
  fi
  if [ "$actual_sha" != "$expected_sha" ]; then
    echo "SHA-256 mismatch for $filename: expected $expected_sha, got $actual_sha" >&2
    exit 1
  fi
  if command -v sqlite3 >/dev/null 2>&1; then
    integrity=$(sqlite3 "$tmp_path" "PRAGMA integrity_check;" | head -n 1)
    if [ "$integrity" != "ok" ]; then
      echo "SQLite integrity check failed: $integrity" >&2
      exit 1
    fi
  fi
  mv "$tmp_path" "$local_path"
  echo "Saved verified off-host backup: $local_path (SHA-256: $actual_sha)"
fi

find "$dest_dir" -maxdepth 1 -type f -name 'tokenlens-*.sqlite' -mtime +"$keep_days" -delete
