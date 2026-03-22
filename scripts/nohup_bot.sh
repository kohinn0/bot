#!/usr/bin/env bash
# Bot háttérben – SSH kilépés után is fut (nohup).
# Használat: bash scripts/nohup_bot.sh
# Leállítás: pkill -f sebessegbot_rs   (vagy: kill $(pgrep -f sebessegbot_rs))

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BOT_DIR="${BOT_DIR_OVERRIDE:-$ROOT/rust_bot}"

if [[ ! -x "$BOT_DIR/target/release/sebessegbot_rs" ]]; then
  echo "❌ Nincs release bináris: $BOT_DIR/target/release/sebessegbot_rs"
  echo "   Futtasd: cd $BOT_DIR && cargo build --release"
  exit 1
fi

cd "$BOT_DIR"
LOG="${BOT_LOG:-$BOT_DIR/bot.log}"
echo "🚀 Indítás (nohup) → log: $LOG"
nohup ./target/release/sebessegbot_rs >>"$LOG" 2>&1 &
echo $! >"$BOT_DIR/bot.pid"
echo "✅ PID: $(cat "$BOT_DIR/bot.pid")  |  tail -f $LOG"
