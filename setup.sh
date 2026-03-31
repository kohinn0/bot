#!/usr/bin/env bash
# SebessegBot – egy szkript: telepítés, frissítés, fordítás, indítás / állapot.
# Használat: ./setup.sh help

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUST_BOT="$ROOT/rust_bot"
BIN="$RUST_BOT/target/release/sebessegbot_rs"
LOG="${BOT_LOG:-$RUST_BOT/bot.log}"
PID_FILE="$RUST_BOT/bot.pid"

die() { echo "Hiba: $*" >&2; exit 1; }

have_cmd() { command -v "$1" >/dev/null 2>&1; }

ensure_cargo_env() {
  if [[ -f "${HOME}/.cargo/env" ]]; then
    # shellcheck source=/dev/null
    source "${HOME}/.cargo/env"
  fi
}

install_os_packages() {
  if have_cmd apt-get; then
    sudo apt-get update -qq
    sudo apt-get install -y -qq git curl build-essential libssl-dev pkg-config
  elif have_cmd dnf; then
    sudo dnf install -y git curl gcc openssl-devel pkgconf-pkg-config
  else
    echo "Figyelem: nincs apt-get / dnf – telepíts kézzel: git, curl, C fordító, OpenSSL dev, pkg-config."
  fi
}

ensure_rust() {
  ensure_cargo_env
  if have_cmd cargo; then
    echo "Rust (cargo) rendben."
    return
  fi
  if [[ ! -f "${HOME}/.cargo/env" ]] && have_cmd curl; then
    echo "Rust telepítése (rustup)..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    ensure_cargo_env
  fi
  have_cmd cargo || die "cargo nem található. Telepítsd: https://rustup.rs"
}

git_sync() {
  if [[ -d "$ROOT/.git" ]]; then
    echo "Git pull: $ROOT"
    git -C "$ROOT" pull --ff-only --quiet || git -C "$ROOT" pull
  else
    echo "Nincs .git – kihagyva (fejlesztői másolat vagy archívum)."
  fi
}

sync_dotenv() {
  if [[ -f "$ROOT/.env" && ! -f "$RUST_BOT/.env" ]]; then
    cp "$ROOT/.env" "$RUST_BOT/.env"
    echo ".env átmásolva: $ROOT -> $RUST_BOT"
  fi
}

do_build() {
  [[ -d "$RUST_BOT" ]] || die "Nincs rust_bot könyvtár: $RUST_BOT"
  ensure_cargo_env
  echo "Release build: $RUST_BOT"
  (cd "$RUST_BOT" && cargo build --release)
  echo "Kész: $BIN"
}

cmd_install() {
  install_os_packages
  ensure_rust
  git_sync
  sync_dotenv
  do_build
  echo ""
  echo "Telepítés kész."
  cmd_help_tail
}

cmd_update() {
  ensure_rust
  git_sync
  sync_dotenv
  do_build
  echo ""
  echo "Frissítés kész."
  cmd_help_tail
}

cmd_status() {
  echo "=== systemd: sebessegbot ==="
  if systemctl list-unit-files 2>/dev/null | grep -q '^sebessegbot\.service'; then
    if systemctl is-active --quiet sebessegbot 2>/dev/null; then
      echo "Állapot: aktív"
    else
      echo "Állapot: nem fut"
    fi
    systemctl is-enabled sebessegbot 2>/dev/null || true
  else
    echo "(nincs sebessegbot.service a systemd-ben)"
  fi
  echo ""
  echo "=== folyamat: sebessegbot_rs ==="
  pgrep -af sebessegbot_rs 2>/dev/null || echo "(nincs ilyen folyamat)"
  echo ""
  echo "=== utolsó log sorok: $LOG ==="
  if [[ -f "$LOG" ]]; then
    tail -n 15 "$LOG"
  else
    echo "(nincs log fájl)"
  fi
}

cmd_start() {
  if systemctl is-active --quiet sebessegbot 2>/dev/null; then
    die "A bot systemd alatt fut. Használd: sudo systemctl stop sebessegbot – ne indítsd kétszer."
  fi
  [[ -x "$BIN" ]] || die "Nincs release bináris. Futtasd: $0 build"
  if pgrep -f 'sebessegbot_rs' >/dev/null 2>&1; then
    echo "Meglévő sebessegbot_rs leállítása..."
    pkill -f 'target/release/sebessegbot_rs' || true
    sleep 2
  fi
  mkdir -p "$(dirname "$LOG")"
  echo "$(date -Iseconds) — nohup start" >>"$LOG"
  pushd "$RUST_BOT" >/dev/null
  nohup "$BIN" >>"$LOG" 2>&1 &
  echo $! >"$PID_FILE"
  popd >/dev/null
  echo "Indítva. PID: $(cat "$PID_FILE")  |  log: tail -f $LOG"
}

cmd_stop() {
  if systemctl is-active --quiet sebessegbot 2>/dev/null; then
    echo "A bot systemd alatt fut. Állítsd le: sudo systemctl stop sebessegbot"
    exit 1
  fi
  if pkill -f 'target/release/sebessegbot_rs' 2>/dev/null; then
    echo "sebessegbot_rs leállítva."
  else
    echo "Nem futott sebessegbot_rs (vagy systemd kezeli)."
  fi
}

cmd_help_tail() {
  echo "Parancsok:"
  echo "  $0 update   – git pull + újrafordítás"
  echo "  $0 build    – csak fordítás"
  echo "  $0 repo-map – repo térkép generálás (docs/repo-map.md)"
  echo "  $0 status   – systemd / folyamat / log"
  echo "  $0 start    – háttér (nohup), ne systemd mellett"
  echo "  $0 stop     – nohup leállítás"
  echo ".env helye:   $RUST_BOT/.env"
  echo "Stratégia:   $RUST_BOT/strategy_maker.json"
  echo "systemd:     sudo cp $ROOT/deploy/sebessegbot.service /etc/systemd/system/  (User + útvonalak!)"
}

cmd_help() {
  echo "SebessegBot – telepítés és üzemeltetés egy szkriptből."
  echo ""
  echo "Használat: $0 <parancs>"
  echo ""
  echo "  install   Első telepítés: OS csomagok (apt/dnf), Rust, git pull, .env másolás, release build"
  echo "  update    Frissítés: git pull + release build"
  echo "  build     Csak cargo build --release"
  echo "  repo-map  AI-hoz repo térkép generálás (docs/repo-map.md)"
  echo "  status    systemd / folyamat / log utolsó sorai"
  echo "  start     Bot indítása nohup-pal (rust_bot mappából)"
  echo "  stop      nohup folyamat leállítása"
  echo "  help      Ez a súgó"
  echo ""
  cmd_help_tail
}

main() {
  local sub="${1:-help}"
  case "$sub" in
    install) cmd_install ;;
    update)  cmd_update ;;
    build)   ensure_rust; do_build ;;
    repo-map) python3 "$ROOT/scripts/repo_map.py" --root "$ROOT" --output "docs/repo-map.md" ;;
    status)  cmd_status ;;
    start)   cmd_start ;;
    stop)    cmd_stop ;;
    help|-h|--help) cmd_help ;;
    *) die "Ismeretlen parancs: $sub  (futtasd: $0 help)" ;;
  esac
}

main "$@"
