#!/bin/sh
# Restore drill: restores the newest backup into a scratch directory with the
# same `restore` command a real recovery uses, starts a throwaway server on it
# and checks it answers. The running server and ./data are never touched.
# Run as root: the backups belong to the container user and are mode 600.
set -eu
cd "$(dirname "$0")"

latest=$(ls -1t backups/tokenlens-*.sqlite 2>/dev/null | head -n 1 || true)
if [ -z "$latest" ]; then
  echo "No backups yet; run ./backup.sh first." >&2
  exit 1
fi

drill=$(mktemp -d)
container="tokenlens-restore-drill-$$"
backup_owner=$(stat -c '%u:%g' "$latest" 2>/dev/null || stat -f '%u:%g' "$latest")
run_user="$(id -u):$(id -g)"
if [ "$(id -u)" -eq 0 ]; then
  chown "$backup_owner" "$drill"
  run_user="$backup_owner"
fi
cleanup() {
  docker rm -f "$container" >/dev/null 2>&1 || true
  rm -rf "$drill"
}
trap cleanup EXIT INT TERM
echo "Drill on $latest"
docker run --rm --user "$run_user" --network none \
  --read-only --tmpfs /tmp --cap-drop ALL --security-opt no-new-privileges:true \
  -v "$PWD/backups:/app/backups:ro" -v "$drill:/app/data" \
  tokenlens-server:local tokenlens-server restore "/app/backups/$(basename "$latest")"

docker run -d --name "$container" --user "$run_user" \
  --network none --read-only --tmpfs /tmp --cap-drop ALL --security-opt no-new-privileges:true \
  -v "$drill:/app/data" tokenlens-server:local >/dev/null

tries=0
until docker exec "$container" tokenlens-server healthcheck 2>/dev/null; do
  tries=$((tries + 1))
  if [ "$tries" -ge 15 ]; then
    echo "The restored copy did not start:" >&2
    docker logs "$container" >&2 || true
    exit 1
  fi
  sleep 1
done

docker exec "$container" tokenlens-server verify-backup /app/data/tokenlens.sqlite
docker exec "$container" tokenlens-server device list
echo "Restore drill passed."
