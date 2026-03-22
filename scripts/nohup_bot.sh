#!/usr/bin/env bash
# Bot háttérben – SSH kilépés után is fut (nohup).
# Meglévő példányt leállít (ne legyen dupla bot).
#
# Használat: bash scripts/nohup_bot.sh
# Állapot:   bash scripts/bot_status.sh
# Leállítás: bash scripts/stop_bot.sh

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BOT_DIR="${BOT_DIR_OVERRIDE:-$ROOT/rust_bot}"
BIN="$BOT_DIR/target/release/sebessegbot_rs"
LOG="${BOT_LOG:-$BOT_DIR/bot.log}"

if [[ ! -x "$BIN" ]]; then
  echo "❌ Nincs release bináris: $BIN"
  echo "   cd $BOT_DIR && cargo build --release"
  exit 1
fi

# Régi nohup / kézi futás leállítása (systemd-et NE öld: systemctl stop)
if pgrep -f 'sebessegbot_rs' >/dev/null 2>&1; then
  if systemctl is-active --quiet sebessegbot 2>/dev/null; then
    echo "⚠️  systemd alatt fut a sebessegbot. Ne indítsd nohup-pal is."
    echo "    Állapot: sudo systemctl status sebessegbot"
    exit 1
  fi
  echo "🛑 Meglévő sebessegbot_rs leállítása..."
  pkill -f 'target/release/sebessegbot_rs' || true
  sleep 2
fi

cd "$BOT_DIR"
echo "$(date -Iseconds) — nohup start" >>"$LOG"
nohup "$BIN" >>"$LOG" 2>&1 &
echo $! >"$BOT_DIR/bot.pid"
echo "✅ PID: $(cat "$BOT_DIR/bot.pid")"
echo "   Log: tail -f $LOG"
