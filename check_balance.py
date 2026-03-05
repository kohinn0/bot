"""
Polymarket Proxy Wallet - USDC Balance Check
Lekérdezi az egyenleget a Polygon blokkláncon keresztül.
"""
import os
import json
import requests
from dotenv import load_dotenv

load_dotenv()

# USDC contract on Polygon
USDC_CONTRACT = "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174"  # USDC.e (bridged)
USDC_V2_CONTRACT = "0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359"  # Native USDC

# Wallet address (proxy/funder)
WALLET = os.getenv("FUNDER_ADDRESS", "")
# Alchemy key expired → public Polygon RPC fallback
RPC_URL = "https://polygon-bor-rpc.publicnode.com"

def get_erc20_balance(rpc_url: str, token_contract: str, wallet: str) -> float:
    """Query ERC-20 token balance via eth_call"""
    # balanceOf(address) selector = 0x70a08231
    # Pad wallet address to 32 bytes
    wallet_padded = wallet.lower().replace("0x", "").zfill(64)
    data = f"0x70a08231{wallet_padded}"
    
    payload = {
        "jsonrpc": "2.0",
        "method": "eth_call",
        "params": [
            {"to": token_contract, "data": data},
            "latest"
        ],
        "id": 1
    }
    
    resp = requests.post(rpc_url, json=payload, timeout=10)
    result = resp.json().get("result", "0x0")
    
    # Convert hex to integer, then to USDC (6 decimals)
    balance_raw = int(result, 16)
    return balance_raw / 1e6


def get_matic_balance(rpc_url: str, wallet: str) -> float:
    """Query native MATIC/POL balance"""
    payload = {
        "jsonrpc": "2.0",
        "method": "eth_getBalance",
        "params": [wallet, "latest"],
        "id": 2
    }
    
    resp = requests.post(rpc_url, json=payload, timeout=10)
    result = resp.json().get("result", "0x0")
    balance_raw = int(result, 16)
    return balance_raw / 1e18


if __name__ == "__main__":
    print("=" * 55)
    print("  POLYMARKET PROXY WALLET - EGYENLEG LEKÉRDEZÉS")
    print("=" * 55)
    
    if not WALLET:
        print("❌ FUNDER_ADDRESS nincs beállítva a .env fájlban!")
        exit(1)
    
    print(f"\n📍 Wallet: {WALLET}")
    print(f"🔗 RPC: {RPC_URL[:50]}...")
    
    # 1. USDC.e (bridged) balance
    print("\n── USDC Egyenleg ──")
    try:
        usdc_e = get_erc20_balance(RPC_URL, USDC_CONTRACT, WALLET)
        print(f"  USDC.e (Bridged):  ${usdc_e:,.2f}")
    except Exception as e:
        print(f"  USDC.e: ❌ Hiba: {e}")
        usdc_e = 0
    
    # 2. Native USDC balance
    try:
        usdc_native = get_erc20_balance(RPC_URL, USDC_V2_CONTRACT, WALLET)
        print(f"  USDC   (Native):   ${usdc_native:,.2f}")
    except Exception as e:
        print(f"  USDC Native: ❌ Hiba: {e}")
        usdc_native = 0
    
    total_usdc = usdc_e + usdc_native
    print(f"  ─────────────────────────")
    print(f"  ÖSSZESEN:          ${total_usdc:,.2f} USDC")
    
    # 3. MATIC/POL balance (for gas, if needed)
    print("\n── MATIC/POL Egyenleg ──")
    try:
        matic = get_matic_balance(RPC_URL, WALLET)
        print(f"  MATIC: {matic:,.4f}")
    except Exception as e:
        print(f"  MATIC: ❌ Hiba: {e}")
    
    print("\n" + "=" * 55)
    print("BALANCE_CHECK_DONE")
