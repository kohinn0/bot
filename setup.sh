#!/bin/bash
# =============================================================
# SebessegBot – Rust-only telepítő belépőpont
# Használat: bash setup.sh
# =============================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [ ! -f "$SCRIPT_DIR/setup_rust.sh" ]; then
  echo "❌ setup_rust.sh nem található a projekt gyökerében."
  exit 1
fi

echo "➡️ Python bot ág eltávolítva, Rust telepítő indul..."
exec bash "$SCRIPT_DIR/setup_rust.sh"
