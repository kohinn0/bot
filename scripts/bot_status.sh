#!/usr/bin/env bash
# Gyors ellenőrzés: fut-e a bot, systemd vagy nohup

echo "=== systemd sebessegbot ==="
if systemctl list-unit-files 2>/dev/null | grep -q '^sebessegbot.service'; then
  systemctl is-active sebessegbot 2>/dev/null && echo "Állapot: AKTÍV" || echo "Állapot: nem fut"
  systemctl is-enabled sebessegbot 2>/dev/null || true
else
  echo "(nincs telepítve sebessegbot.service)"
fi

echo ""
echo "=== folyamat (sebessegbot_rs) ==="
pgrep -af sebessegbot_rs || echo "(nincs ilyen folyamat)"

echo ""
echo "=== utolsó log sorok (rust_bot/bot.log, ha van) ==="
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOG="$ROOT/rust_bot/bot.log"
if [[ -f "$LOG" ]]; then
  tail -n 15 "$LOG"
else
  echo "(nincs $LOG)"
fi
