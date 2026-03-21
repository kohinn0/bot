# ⚡ SebessegBot

**Ultra-low latency, mean-reversion maker bot a Hyperliquid perpetuálison.**

Z-score alapú létra stratégia, kétlépcsős WebSocket védelem, automatikus VPS deploy.

---

## 🚀 Telepítés (VPS – Rust)

```bash
git clone https://github.com/kohinn0/bot ~/sebessegbot
cd ~/sebessegbot
bash setup.sh
```

A `setup.sh` a teljes Rust telepítőt futtatja.
Telepíti a Rust toolchaint + build függőségeket, majd lefordítja a botot release módban.

---

## ⚙️ Kézi indítás (Rust)

```bash
cp .env.example .env   # majd add meg a PRIVATE_KEY-t

bash setup.sh
cd rust_bot
cargo build --release
./target/release/sebessegbot_rs
```

---

## � .env konfiguráció

```env
PRIVATE_KEY=0x...       # Hyperliquid privát kulcs
DRY_RUN=true            # true = szimuláció | false = éles
IS_MAINNET=true         # mainnet / testnet
STARTING_EQUITY_USD=100 # fallback: méret + drawdown, ha nincs HL egyenleg (pl. DRY_RUN)
USE_WALLET_BALANCE_FOR_SIZING=true  # élesben: méret a HL számla accountValue alapján
WALLET_EQUITY_REFRESH_SEC=30        # HL egyenleg frissítése (másodperc)
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

## 🧪 Diagnosztika

Rust build után futtass rövid ellenőrzést:

- `python test_ws.py` (WS kapcsolat smoke test)
- induláskor figyeld a `WS POST ERROR` / `WS POST FEEDBACK` sorokat

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

## 📁 Struktúra (aktuális)

```
rust_bot/src/main.rs           # Főprogram (Tokio event loop)
rust_bot/src/network/feed.rs   # HL WebSocket feed + order post
rust_bot/src/network/client.rs # HL REST kliens
rust_bot/src/logic/order_manager.rs # Létra, TP/SL, payload wire
rust_bot/src/logic/signer.rs   # EIP-712 aláírás
strategy_maker.json    # Stratégia paraméterek
setup.sh               # Rust VPS telepítő/fordító
```
