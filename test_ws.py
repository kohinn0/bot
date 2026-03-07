import os
import time
from dotenv import load_dotenv
from eth_account import Account
from hyperliquid.info import Info
from hyperliquid.utils import constants

load_dotenv()
priv_key = os.getenv("PRIVATE_KEY")
if not priv_key:
    # Just a random wallet for testing connection syntax
    wallet = Account.create()
    print(f"Using random fallback wallet: {wallet.address}")
else:
    wallet = Account.from_key(priv_key)
    print(f"Wallet: {wallet.address}")

info = Info(constants.MAINNET_API_URL, skip_ws=False)

def on_user_event(msg):
    print(f"\n[WS USER EVENT] {msg}\n")

print("Subscribing to userEvents...")
# According to HL docs, user events requires "user": address
info.subscribe({"type": "userEvents", "user": wallet.address}, on_user_event)

print("Listening for 15 seconds...")
try:
    for i in range(15):
        time.sleep(1)
        print(".", end="", flush=True)
except KeyboardInterrupt:
    pass

print("\nDone")
