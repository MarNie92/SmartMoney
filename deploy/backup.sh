#!/bin/sh
# Consistent SQLite backup (safe while the daemon is running), keeping the
# seven newest. Run daily by smartmoney-backup.timer.
set -eu

DB="${SMARTMONEY_DATABASE_PATH:-/var/lib/smartmoney/smartmoney.db}"
DIR="${SMARTMONEY_BACKUP_DIR:-/var/lib/smartmoney/backups}"
KEEP="${SMARTMONEY_BACKUP_KEEP:-7}"

if [ ! -f "$DB" ]; then
    echo "no database at $DB yet; nothing to back up"
    exit 0
fi

mkdir -p "$DIR"
out="$DIR/smartmoney-$(date -u +%Y%m%d-%H%M).db"
# .backup copies a consistent snapshot even with concurrent writers.
sqlite3 "$DB" ".backup '$out'"
gzip -f "$out"
echo "wrote $out.gz"

# Delete all but the newest $KEEP archives.
ls -1t "$DIR"/smartmoney-*.db.gz 2>/dev/null | tail -n "+$((KEEP + 1))" | while read -r old; do
    rm -f "$old"
    echo "removed $old"
done
