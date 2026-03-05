# ⚡ SebessegBot

**Ultra-low latency, mean-reversion maker bot a Hyperliquid perpetuálison.**

Z-score alapú létra stratégia, kétlépcsős WebSocket védelem, automatikus VPS deploy.

---

## 🚀 Telepítés (VPS – egyetlen parancs)

```bash
git clone https://github.com/kohinn0/bot ~/sebessegbot
cd ~/sebessegbot
bash setup.sh
```

A script interaktívan bekéri a privát kulcsot, lefuttatja a backend tesztet,
és regisztrálja a botot `systemd` alá – szerver újraindítás után is automatikusan elindul.

---

## ⚙️ Kézi indítás

```bash
python3 -m venv venv && source venv/bin/activate
pip install -r requirements.txt
cp .env.example .env   # majd add meg a PRIVATE_KEY-t

python test_backend.py  # ← mindig futtasd először!
python bot.py           # dry run (szimuláció)
python bot.py --live    # éles kereskedés
```

---

## � .env konfiguráció

```env
PRIVATE_KEY=0x...       # Hyperliquid privát kulcs
DRY_RUN=true            # true = szimuláció | false = éles
```

> ⚠️ A `.env` fájl nincs a repóban – sosem kerüljön commitba.

---

## 🖥️ VPS kezelés

```bash
sudo systemctl start sebessegbot    # indítás
sudo systemctl stop sebessegbot     # leállítás
sudo systemctl status sebessegbot   # státusz
journalctl -u sebessegbot -f        # élő napló
bash setup.sh                       # frissítés
```

---

## 🧪 Backend teszt

A `test_backend.py` az alábbiak rendben létét ellenőrzi mielőtt a bot elindul:

- Python 3.10+ és függőségek
- `.env` fájl és `PRIVATE_KEY`
- Összes Python modul betölthetősége
- `strategy_maker.json` konfig (leverage, isolated margin)
- Hyperliquid REST API elérhetősége
- WebSocket L2 feed 3 másodperces élő tesztje
- Wallet inicializálás

---

## 🛡️ Állapotgép (FSM)

```
IDLE → ARMED → LADDER_PLACED → IN_POSITION → EXITING → COOLDOWN
                                                 ↕
                                            RECOVERING
```

| Állapot | Leírás |
|---|---|
| `ARMED` | Jelet vár, nem kereskedik |
| `LADDER_PLACED` | Post-only limit létra aktív |
| `IN_POSITION` | Pozíció nyitva, TP order könyvben |
| `EXITING` | Kilépés folyamatban |
| `COOLDOWN` | Várakozás következő belépés előtt |
| `RECOVERING` | WebSocket kiesés utáni cleanup + ellenőrzés |

### Hálózati védelem

| Feed késés | Reakció |
|---|---|
| **1–3 mp** | Warning – nincs új belépés, meglévő pozíció tartva |
| **3+ mp** | Panic Cancel – minden order törölve, pozíció piaci áron zárva |
| **Feed visszatér** | Recovery – API ellenőrzés → 30s cooldown → `ARMED` |

---

## 📁 Struktúra

```
bot.py                 # Főprogram, FSM
order_manager.py       # Létra + TP kezelés
signal_engine.py       # Z-score, volatilitás, jelgenerátor
hyperliquid_feed.py    # L2 WebSocket (ultra-low latency)
hyperliquid_client.py  # HL REST/SDK kliens
config.py              # Konfig loader
bot_logger.py          # Naplózás
strategy_maker.json    # Stratégia paraméterek
test_backend.py        # Öndiagnosztika
setup.sh               # VPS telepítő
```
