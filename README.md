# SebessegBot

Mean-reversion maker stratégia a Hyperliquid perpeken (Rust / Tokio).

---

## Telepítés és frissítés (egy szkript)

A repó gyökerében minden itt van: **`setup.sh`**.

```bash
git clone https://github.com/kohinn0/bot ~/sebessegbot
cd ~/sebessegbot
chmod +x setup.sh
./setup.sh install    # első alkalom: OS csomagok (apt/dnf), Rust, git pull, fordítás
```

Később, ha csak kód + újrafordítás kell:

```bash
./setup.sh update
```

További parancsok:

| Parancs | Mit csinál |
|--------|------------|
| `./setup.sh install` | Teljes első telepítés |
| `./setup.sh update` | `git pull` + `cargo build --release` |
| `./setup.sh build` | Csak fordítás |
| `./setup.sh status` | systemd / folyamat / `rust_bot/bot.log` utolsó sorai |
| `./setup.sh start` | Háttérindítás nohup-pal (ne systemd mellett) |
| `./setup.sh stop` | Nohup folyamat leállítása |
| `./setup.sh help` | Súgó |

Ha a gyökérben van `.env`, de `rust_bot/.env` nincs, az `install` / `update` átmásolja.

---

## Környezet

- **`.env`** helye: `rust_bot/.env` (a bot innen futva ezt olvassa, ha `dotenv` a `rust_bot` mappában van).
- Sablon: `.env.example` a repó gyökerében.

```env
PRIVATE_KEY=0x...
IS_MAINNET=true
# … lásd .env.example
```

---

## systemd (ajánlott élesben)

```bash
sudo cp deploy/sebessegbot.service /etc/systemd/system/
# szerkeszd: User, WorkingDirectory, ExecStart útvonalak
sudo systemctl daemon-reload
sudo systemctl enable --now sebessegbot
sudo systemctl status sebessegbot
journalctl -u sebessegbot -f
```

Új bináris deploy után: `./setup.sh update`, majd `sudo systemctl restart sebessegbot`.

---

## Struktúra

```
rust_bot/src/main.rs
rust_bot/strategy_maker.json   # stratégia
rust_bot/.env                  # titkok (nem commit)
setup.sh                       # telepítés / frissítés / build / státusz / nohup
deploy/sebessegbot.service
```

---

## Diagnosztika

- `./setup.sh status`
- induláskor: `WS POST ERROR` / feed sorok a logban
