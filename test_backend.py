#!/usr/bin/env python3
# pyre-ignore-all-errors
"""
SebessegBot - Backend Öndiagnosztika
Futtatás: python test_backend.py
Ez a script ellenőrzi, hogy minden rendben van-e MIELŐTT live módba váltanál.
"""

import os
import sys
import time
import json
import importlib

PASS = "\033[92m✅ PASS\033[0m"
FAIL = "\033[91m❌ FAIL\033[0m"
WARN = "\033[93m⚠️  WARN\033[0m"
INFO = "\033[94mℹ️  INFO\033[0m"

results = []

def check(name: str, ok: bool, detail: str = ""):
    status = PASS if ok else FAIL
    print(f"  {status}  {name}" + (f" → {detail}" if detail else ""))
    results.append((name, ok))

def section(title: str):
    print(f"\n\033[1m{'='*55}\033[0m")
    print(f"\033[1m  {title}\033[0m")
    print(f"\033[1m{'='*55}\033[0m")

# ──────────────────────────────────────────────
section("1. Python & Függőségek")

check("Python 3.10+", sys.version_info >= (3, 10), f"v{sys.version_info.major}.{sys.version_info.minor}")

deps = {
    "dotenv": "python-dotenv",
    "requests": "requests",
    "websockets": "websockets",
    "hyperliquid": "hyperliquid-python-sdk",
}
for mod, pkg in deps.items():
    try:
        importlib.import_module(mod)
        check(f"Import: {pkg}", True)
    except ImportError:
        check(f"Import: {pkg}", False, f"Hiányzik! Futtasd: pip install {pkg}")

# ──────────────────────────────────────────────
section("2. .env Konfiguráció")

env_path = os.path.join(os.path.dirname(__file__), ".env")
check(".env fájl létezik", os.path.exists(env_path), env_path)

if os.path.exists(env_path):
    from dotenv import load_dotenv
    load_dotenv(env_path)

pk = os.environ.get("PRIVATE_KEY", "")
check("PRIVATE_KEY megadva", bool(pk) and pk != "0x_id_be_a_sajat_private_kulcsodat",
      "Nincs beállítva!" if not pk else f"0x...{pk[-4:]}")
check("DRY_RUN=true (biztonságos)", os.environ.get("DRY_RUN", "true").lower() == "true",
      f"DRY_RUN={os.environ.get('DRY_RUN', 'nincs megadva')}")

# ──────────────────────────────────────────────
section("3. Belső Modulok Betöltése")

modules_to_test = ["bot_logger", "config", "hyperliquid_feed", "hyperliquid_client",
                   "order_manager", "signal_engine"]
for m in modules_to_test:
    try:
        importlib.import_module(m)
        check(f"Modul: {m}", True)
    except Exception as e:
        check(f"Modul: {m}", False, str(e)[:80])

# ──────────────────────────────────────────────
section("4. Stratégia Config Ellenőrzés")

cfg_file = "strategy_maker.json"
try:
    with open(cfg_file) as f:
        cfg = json.load(f)
    check("strategy_maker.json betöltve", True)
    check("leverage section létezik", "leverage" in cfg.get("risk_management", {}),
          "risk_management.leverage hiányzik!")
    lev = cfg.get("risk_management", {}).get("leverage", {})
    check(f"max_leverage <= 10x", lev.get("max_leverage", 99) <= 10,
          f"Jelenleg: {lev.get('max_leverage')}x")
    check(f"cross_margin = false (isolated)", not lev.get("cross_margin", True),
          "Állítsd false-ra a strategiában!")
except Exception as e:
    check("strategy_maker.json", False, str(e)[:80])

# ──────────────────────────────────────────────
section("5. Hyperliquid Hálózati Kapcsolat")

try:
    import requests
    resp = requests.post(
        "https://api.hyperliquid.xyz/info",
        json={"type": "meta"},
        timeout=5
    )
    check("HL REST API elérhető", resp.status_code == 200,
          f"HTTP {resp.status_code}")
    if resp.status_code == 200:
        universe = resp.json().get("universe", [])
        btc_found = any(c.get("name") == "BTC" for c in universe)
        check("BTC piac megtalálható", btc_found)
except Exception as e:
    check("HL REST API", False, str(e)[:80])

# ──────────────────────────────────────────────
section("6. WebSocket Feed Teszt (3 másodperc)")

try:
    from hyperliquid_feed import HyperliquidFeed
    feed = HyperliquidFeed(coin="BTC")
    feed.start()
    time.sleep(3)
    price = feed.get_current_price()
    stale = feed.get_staleness_sec()
    feed.stop()
    check("WebSocket feed él", price is not None and price > 0,
          f"BTC ár: ${price:,.0f}" if price else "Nincs ár adat!")
    check("Feed freshness < 2s", stale < 2.0, f"Késés: {stale:.2f}s")
except Exception as e:
    check("WebSocket feed", False, str(e)[:80])

# ──────────────────────────────────────────────
section("7. Hyperliquid Kliens Inicializálás (DRY RUN)")

if pk and pk != "0x_id_be_a_sajat_private_kulcsodat":
    try:
        from hyperliquid_client import HyperliquidClient
        client = HyperliquidClient(dry_run=True)
        check("HyperliquidClient init", client is not None)
        if client.wallet:
            check("Wallet cím generálva", True, client.wallet.address)
        else:
            check("Wallet cím generálva", False, "PRIVATE_KEY hibás?")
    except Exception as e:
        check("HyperliquidClient", False, str(e)[:80])
else:
    print(f"  {WARN}  Kliens teszt kihagyva – PRIVATE_KEY nincs megadva")

# ──────────────────────────────────────────────
section("🏁 Eredmény")

total = len(results)
passed = sum(1 for _, ok in results if ok)
failed = total - passed

print(f"\n  {'='*40}")
print(f"  Összesen: {total} teszt")
print(f"  {PASS}: {passed}")
if failed > 0:
    print(f"  {FAIL}: {failed}")
    print(f"\n  \033[91m⛔ Bot NEM indítható – javítsd a hibákat!\033[0m")
    sys.exit(1)
else:
    print(f"\n  \033[92m🚀 Minden rendben! Bot készen áll az indításra.\033[0m")
    print(f"  Dry run indítás:  python bot.py")
    print(f"  Live indítás:      python bot.py --live")
    sys.exit(0)
