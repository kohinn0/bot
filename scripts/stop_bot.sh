#!/usr/bin/env bash
# Nohup / kézi futás leállítása. Systemd: sudo systemctl stop sebessegbot

set -euo pipefail

if systemctl is-active --quiet sebessegbot 2>/dev/null; then
  echo "A bot systemd alatt fut. Futtasd: sudo systemctl stop sebessegbot"
  exit 1
fi

if pkill -f 'target/release/sebessegbot_rs' 2>/dev/null; then
  echo "✅ sebessegbot_rs leállítva (pkill)."
else
  echo "Nem futott sebessegbot_rs folyamat (vagy systemd kezeli)."
fi
