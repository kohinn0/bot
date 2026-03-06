#!/bin/bash
# =============================================================
# SebessegBot – Egyetlen-parancs VPS Telepítő
# Használat: curl -sSL <raw_github_link>/setup.sh | bash
# Vagy: bash setup.sh
# =============================================================

set -e  # Kilép, ha bármilyen parancs meghibásodik

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

BOT_DIR="$HOME/sebessegbot"
SERVICE_NAME="sebessegbot"
PYTHON_MIN="3.10"

echo -e "${BLUE}"
echo "╔══════════════════════════════════════════════╗"
echo "║        SebessegBot – VPS Telepítő           ║"
echo "║        Hyperliquid Maker Strategy           ║"
echo "╚══════════════════════════════════════════════╝"
echo -e "${NC}"

# ── 1. System frissítés ──────────────────────────
echo -e "${YELLOW}[1/7] System csomagok frissítése...${NC}"
sudo apt-get update -qq
sudo apt-get install -y -qq python3 python3-pip python3-venv git curl

# Python verzió ellenőrzés
PY_VER=$(python3 --version 2>&1 | grep -oP '\d+\.\d+')
echo -e "${GREEN}✅ Python $PY_VER telepítve${NC}"

# ── 2. Bot mappa ─────────────────────────────────
echo -e "${YELLOW}[2/7] Bot könyvtár előkészítése: $BOT_DIR${NC}"
mkdir -p "$BOT_DIR"

# Ha már létezik a repo, csak frissítjük
if [ -d "$BOT_DIR/.git" ]; then
    echo "  → Repo már létezik, frissítés (git pull)..."
    cd "$BOT_DIR" && git pull --quiet
else
    echo "  → Kérjük add meg a GitHub repo URL-t (pl. https://github.com/felhasznalo/sebessegbot):"
    read -r REPO_URL
    git clone "$REPO_URL" "$BOT_DIR"
    cd "$BOT_DIR"
fi
cd "$BOT_DIR"

# ── 3. Python virtuális környezet ────────────────
echo -e "${YELLOW}[3/7] Python virtual environment létrehozása...${NC}"
python3 -m venv venv
source venv/bin/activate
pip install --upgrade pip --quiet
pip install -r requirements.txt --quiet
echo -e "${GREEN}✅ Függőségek telepítve${NC}"

# ── 4. .env fájl beállítása ───────────────────────
echo -e "${YELLOW}[4/7] API kulcsok beállítása...${NC}"
if [ ! -f ".env" ]; then
    cp .env.example .env
    echo ""
    echo -e "${RED}⚠️  Add meg a Hyperliquid privát kulcsodat:${NC}"
    read -rsp "  PRIVATE_KEY (0x...): " PK_INPUT
    echo ""
    sed -i "s|0x_id_be_a_sajat_private_kulcsodat|$PK_INPUT|g" .env

    echo -e "${YELLOW}  Élesen futtassuk? (true=szimuláció / false=ÉLES)${NC}"
    read -rp "  DRY_RUN [true]: " DRY_RUN_INPUT
    DRY_RUN_INPUT=${DRY_RUN_INPUT:-true}
    sed -i "s|DRY_RUN=true|DRY_RUN=$DRY_RUN_INPUT|g" .env
    echo -e "${GREEN}✅ .env mentve${NC}"
else
    echo -e "${GREEN}  → .env már létezik, nem írjuk felül${NC}"
fi

# ── 5. Backend teszt ─────────────────────────────
echo -e "${YELLOW}[5/7] Backend öndiagnosztika futtatása...${NC}"
source venv/bin/activate
python test_backend.py
if [ $? -ne 0 ]; then
    echo -e "${RED}❌ Backend teszt MEGBUKOTT. Javítsd a hibákat, majd futtasd újra: bash setup.sh${NC}"
    exit 1
fi

# ── 6. systemd service ───────────────────────────
echo -e "${YELLOW}[6/7] systemd service létrehozása (auto-restart)...${NC}"

sudo tee /etc/systemd/system/${SERVICE_NAME}.service > /dev/null <<EOF
[Unit]
Description=SebessegBot – Hyperliquid Maker Bot
After=network-online.target
Wants=network-online.target
StartLimitIntervalSec=120
StartLimitBurst=5

[Service]
Type=simple
User=$USER
WorkingDirectory=$BOT_DIR
EnvironmentFile=$BOT_DIR/.env
Environment=PYTHONUNBUFFERED=1
ExecStart=$BOT_DIR/venv/bin/python $BOT_DIR/bot.py
Restart=always
RestartSec=5s
KillMode=mixed
TimeoutStopSec=20

# Naplózás journald-ba (lekérdezés: journalctl -u sebessegbot -f)
StandardOutput=journal
StandardError=journal
SyslogIdentifier=sebessegbot

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable "${SERVICE_NAME}"
echo -e "${GREEN}✅ systemd service regisztrálva és engedélyezve${NC}"

# ── 7. Logrotate ─────────────────────────────────
echo -e "${YELLOW}[7/7] Log rotáció beállítása...${NC}"
sudo tee /etc/logrotate.d/${SERVICE_NAME} > /dev/null <<EOF
$BOT_DIR/logs/*.log {
    daily
    rotate 14
    compress
    delaycompress
    missingok
    notifempty
    create 0640 $USER $USER
}
EOF
mkdir -p "$BOT_DIR/logs"
echo -e "${GREEN}✅ Logrotate beállítva (14 napos megőrzés)${NC}"

# ── Kész ─────────────────────────────────────────
echo ""
echo -e "${GREEN}╔══════════════════════════════════════════════╗${NC}"
echo -e "${GREEN}║        ✅ TELEPÍTÉS BEFEJEZVE!               ║${NC}"
echo -e "${GREEN}╚══════════════════════════════════════════════╝${NC}"
echo ""
echo -e "  ${BLUE}Parancsok:${NC}"
echo -e "  Indítás:         ${YELLOW}sudo systemctl start $SERVICE_NAME${NC}"
echo -e "  Leállítás:       ${YELLOW}sudo systemctl stop $SERVICE_NAME${NC}"
echo -e "  Státusz:         ${YELLOW}sudo systemctl status $SERVICE_NAME${NC}"
echo -e "  Élő napló:       ${YELLOW}journalctl -u $SERVICE_NAME -f${NC}"
echo -e "  Bot frissítése:  ${YELLOW}bash $BOT_DIR/setup.sh${NC}"
echo ""
echo -e "  ${YELLOW}⚠️  Ne felejtsd el ellenőrizni a DRY_RUN értékét a .env fájlban!${NC}"
echo ""

read -rp "  Elindítsuk a botot most? [y/N]: " START_NOW
if [[ "$START_NOW" =~ ^[Yy]$ ]]; then
    sudo systemctl start "$SERVICE_NAME"
    echo -e "${GREEN}🚀 Bot elindítva! Napló: journalctl -u $SERVICE_NAME -f${NC}"
fi
