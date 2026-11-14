#!/usr/bin/env bash
# Install or update SmartMoney on a Linux host (built for Oracle Cloud
# Always Free, Ubuntu on Ampere/aarch64). Idempotent: run it again to
# deploy a new version.
#
#   sudo ./deploy/install.sh            # build from this checkout and install
#   sudo ./deploy/install.sh --no-build # install the existing binary only
#
# Secrets are never written by this script; it only creates an empty
# /etc/smartmoney/smartmoney.env (0600) for you to fill in.
set -euo pipefail

SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PREFIX=/opt/smartmoney
DATA=/var/lib/smartmoney
CONF=/etc/smartmoney
SERVICE_USER=smartmoney
BUILD=1

for arg in "$@"; do
    case "$arg" in
        --no-build) BUILD=0 ;;
        *) echo "unknown option: $arg" >&2; exit 2 ;;
    esac
done

if [ "$(id -u)" -ne 0 ]; then
    echo "run with sudo" >&2
    exit 1
fi

echo "==> system packages"
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq build-essential pkg-config sqlite3 git curl ca-certificates

echo "==> service user and directories"
id -u "$SERVICE_USER" >/dev/null 2>&1 || useradd --system --home "$DATA" --shell /usr/sbin/nologin "$SERVICE_USER"
install -d -o "$SERVICE_USER" -g "$SERVICE_USER" -m 750 "$DATA" "$DATA/backups"
install -d -m 755 "$PREFIX" "$PREFIX/bin"
install -d -m 750 "$CONF"

if [ "$BUILD" -eq 1 ]; then
    echo "==> building (this takes a while on first run)"
    # Build as the invoking user so cargo's cache stays in their home.
    BUILD_USER="${SUDO_USER:-root}"
    sudo -u "$BUILD_USER" -H bash -lc "cd '$SRC' && cargo build --release -p smartmoney-daemon"
fi

BIN="$SRC/target/release/smartmoney-daemon"
if [ ! -x "$BIN" ]; then
    echo "missing binary at $BIN (build first, or drop the --no-build flag)" >&2
    exit 1
fi

echo "==> installing binaries"
install -m 755 "$BIN" "$PREFIX/bin/smartmoney-daemon"
install -m 755 "$SRC/deploy/backup.sh" "$PREFIX/bin/backup.sh"

if [ ! -f "$CONF/smartmoney.env" ]; then
    echo "==> creating $CONF/smartmoney.env (fill in your secrets)"
    cat > "$CONF/smartmoney.env" <<'EOF'
# SmartMoney secrets and overrides. Never commit this file.
SMARTMONEY_DATABASE_PATH=/var/lib/smartmoney/smartmoney.db
SMARTMONEY_TELEGRAM_BOT_TOKEN=
SMARTMONEY_TELEGRAM_ALLOWED_CHAT_IDS=
SMARTMONEY_ODDS_API_KEY=
# Dashboard: bind to the Tailscale address and set a token, or keep
# 127.0.0.1 (the daemon refuses a public bind without a token).
SMARTMONEY_DASHBOARD_TOKEN=
SMARTMONEY_DASHBOARD_BIND=127.0.0.1:8080
RUST_LOG=info
EOF
    chmod 600 "$CONF/smartmoney.env"
    chown root:"$SERVICE_USER" "$CONF/smartmoney.env"
    chmod 640 "$CONF/smartmoney.env"
fi

echo "==> systemd units"
install -m 644 "$SRC/deploy/smartmoney.service" /etc/systemd/system/smartmoney.service
install -m 644 "$SRC/deploy/smartmoney-backup.service" /etc/systemd/system/smartmoney-backup.service
install -m 644 "$SRC/deploy/smartmoney-backup.timer" /etc/systemd/system/smartmoney-backup.timer
systemctl daemon-reload
systemctl enable --now smartmoney-backup.timer

if systemctl is-active --quiet smartmoney; then
    echo "==> restarting smartmoney"
    systemctl restart smartmoney
else
    echo
    echo "Installed. Next:"
    echo "  1. sudo nano $CONF/smartmoney.env    # fill in the secrets"
    echo "  2. sudo systemctl enable --now smartmoney"
    echo "  3. journalctl -u smartmoney -f       # watch it run"
fi
