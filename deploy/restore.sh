#!/bin/sh
# Restore a `lineup backup` file into the lineup-data volume. Run on the Docker host:
#
#   docker stop lineup && sh deploy/restore.sh ./lineup-backup.db && docker start lineup
#
# The backup is validated before anything changes. The current database and its WAL/SHM are
# moved aside to /data/pre-restore-<timestamp>/, never deleted.
set -eu

backup=${1:?usage: restore.sh <backup.db>}
volume=${VOLUME:-lineup-data}
[ -f "$backup" ] || { echo "no such file: $backup" >&2; exit 1; }
running=$(docker ps --format '{{.Names}}')   # fails loudly (set -e) if docker is unavailable
if printf '%s\n' "$running" | grep -qx lineup; then
  echo "lineup is running; stop it first (docker stop lineup)" >&2
  exit 1
fi

dir=$(cd "$(dirname "$backup")" && pwd)
name=$(basename "$backup")

docker run --rm -v "$volume:/data" -v "$dir:/in:ro" -e NAME="$name" alpine sh -euc '
  apk add -q sqlite
  src="/in/$NAME"
  [ "$(sqlite3 "$src" "pragma integrity_check")" = ok ] || { echo "backup failed integrity_check" >&2; exit 1; }
  sqlite3 "$src" "select count(*) from channels; select count(*) from playlists;" >/dev/null
  cp "$src" /data/lineup.db.restore
  chown 10001 /data/lineup.db.restore
  keep="/data/pre-restore-$(date +%Y%m%d%H%M%S)"
  mkdir "$keep"
  for f in lineup.db lineup.db-wal lineup.db-shm; do
    if [ -e "/data/$f" ]; then mv "/data/$f" "$keep/"; fi
  done
  mv /data/lineup.db.restore /data/lineup.db
  echo "restored; previous files kept in $keep"
'
