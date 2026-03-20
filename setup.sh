#!/bin/bash
# =============================================================
# SebessegBot RUST – VPS Telepítő és Fordító
# Használat: bash setup.sh
# =============================================================

set -e

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

BOT_DIR="$HOME/sebessegbot"

echo -e "${YELLOW}[1/5] Rendszer frissítése és Linux C/C++ fordítók telepítése...${NC}"
sudo apt-get update -qq
sudo apt-get install -y -qq git curl build-essential libssl-dev pkg-config

echo -e "${YELLOW}[2/5] Rust Compiler telepítése...${NC}"
if ! command -v cargo &> /dev/null; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
    echo -e "${GREEN}✅ Rust telepítve!${NC}"
else
    echo -e "${GREEN}✅ Rust már telepítve van.${NC}"
    source "$HOME/.cargo/env"
fi

echo -e "${YELLOW}[3/5] Rust Bot letöltése / frissítése a GitHubról...${NC}"
if [ ! -d "$BOT_DIR/.git" ]; then
    git clone https://github.com/kohinn0/bot "$BOT_DIR"
else
    cd "$BOT_DIR" && git pull --quiet
fi

# Környezeti változók másolása a Rust bothoz, ha léteznek a réginél
if [ -f "$BOT_DIR/.env" ] && [ ! -f "$BOT_DIR/rust_bot/.env" ]; then
    cp "$BOT_DIR/.env" "$BOT_DIR/rust_bot/.env"
    echo -e "${GREEN}✅ .env konfiguráció áthozva az előző Python botból.${NC}"
fi

cd "$BOT_DIR/rust_bot"

echo -e "${YELLOW}[4/5] Bot fordítása (Release mode - Brutális sebesség)...${NC}"
echo -e "Ez eltarthat 1-2 percig, ne zárd be az ablakot!"
cargo build --release

echo -e "${GREEN}[5/5] ✅ KÉSZ! A Rust bot lefordult. ${NC}"
echo -e "=========================================================="
echo -e "A stratégiához a futtatás gomb:"
echo -e "${YELLOW}cd $BOT_DIR/rust_bot${NC}"
echo -e "${YELLOW}./target/release/sebessegbot_rs${NC}"
echo -e "=========================================================="
