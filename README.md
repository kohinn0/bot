# SebessegBot – Hyperliquid Maker Strategy Bot

Ultra-low latency, mean-reversion maker bot a Hyperliquid perpetuálison.

## ⚡ Egyetlen parancs telepítés (VPS)

```bash
git clone https://github.com/FELHASZNALO/sebessegbot ~/sebessegbot
cd ~/sebessegbot
bash setup.sh
```

A telepítő automatikusan:
- ✅ Telepíti a Python függőségeket
- ✅ Bekéri a privát kulcsot és menti `.env`-be
- ✅ Lefuttatja a backend öndiagnosztikát
- ✅ Regisztrálja a `systemd` service-t (auto-restart ha leáll)
- ✅ Beállítja a log rotációt (14 napos megőrzés)

---

## 🗂️ Projekt struktúra

```
sebessegbot/
├── bot.py                # 🧠 Főprogram, FSM állapotgép
├── order_manager.py      # 📋 Létra + TP orderek kezelése
├── signal_engine.py      # 📡 Z-score, volatilitás, jelgenerátor
├── hyperliquid_feed.py   # ⚡ L2 WebSocket feed (ultra-low latency)
├── hyperliquid_client.py # 🔌 Hyperliquid REST/SDK kliens
├── config.py             # ⚙️  Stratégia konfig loader
├── bot_logger.py         # 📝 Strukturált naplózás
├── check_balance.py      # 💰 Egyenleg lekérdező segédprogram
├── strategy_maker.json   # 📊 Stratégia paraméterek
├── requirements.txt      # 📦 Python függőségek
├── .env.example          # 🔑 Env sablon (ebből csináld a .env-t)
├── test_backend.py       # 🧪 Backend öndiagnosztika
└── setup.sh              # 🛠️  Automatikus VPS telepítő
```

---

## 🔧 Kézi indítás (fejlesztés / debug)

```bash
# Virtuális környezet
python3 -m venv venv && source venv/bin/activate
pip install -r requirements.txt

# .env beállítása
cp .env.example .env
nano .env   # Add meg a PRIVATE_KEY-t

# Backend teszt (futtatsd MINDIG mielőtt live-ba mész!)
python test_backend.py

# Dry run (szimuláció, nincs valódi order)
python bot.py

# Éles kereskedés
python bot.py --live
```

---

## 🚀 VPS kezelés

| Parancs | Mit csinál |
|---|---|
| `sudo systemctl start sebessegbot` | Bot indítása |
| `sudo systemctl stop sebessegbot` | Bot leállítása |
| `sudo systemctl status sebessegbot` | Státusz lekérdezése |
| `journalctl -u sebessegbot -f` | Élő napló követése |
| `bash setup.sh` | Frissítés + újratelepítés |

---

## 🛡️ Stratégia & Kockázatkezelés

**Típus:** Maker-only Ambush Létra (Mean Reversion)  
**Piac:** Hyperliquid BTC perpetual  
**Margin:** Izolált margin (isolated, nem cross!)

### Állapotgép (FSM)

```
IDLE → ARMED → LADDER_PLACED → IN_POSITION → EXITING → COOLDOWN
                                                 ↕
                                            RECOVERING  ← (WebSocket kiesés esetén)
```

### Hálózati védelem (kétlépcsős)

| Késés | Reakció |
|---|---|
| 1–3 mp | ⚠️ Warning: Nincs új pozíció, meglévők tartva |
| 3+ mp | 🚨 Panic Cancel: Minden order törölve, pozíció piaci áron zárva |
| Feed visszatér | 🔄 RECOVERING állapot: API ellenőrzés → 30s cooldown → ARMED |

### Kockázatok

- Max **1x–5x** tőkeáttétel (strategy_maker.json-ban állítható)
- Max **$20** per trade
- **$25** napi vesztési limit
- **60s** cooldown exit után
- Exponenciális **retry** (3x) API timeout esetén

---

## 🔑 Biztonsági megjegyzések

> ⚠️ A `.env` fájlt **SOHA ne commitold** be a Git-be!

A `.gitignore` már tartalmazza a `.env` kizárást. Ellenőrzés:
```bash
cat .gitignore | grep .env
```

---

## 📝 Naplók

```bash
# Rendszer napló (systemd)
journalctl -u sebessegbot --since "1 hour ago"

# Fájl naplók
tail -f ~/sebessegbot/logs/bot.log
```
