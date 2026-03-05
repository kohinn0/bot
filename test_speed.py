"""VPS Speed Test - Binance + Polymarket latency from Amsterdam"""
import time
import requests
import statistics
import socket

print("=" * 55)
print("  VPS SEBESSEG TESZT - Amsterdam")
print("=" * 55)

# 1. Network ping
print("\n[1] NETWORK PING")
targets = {
    "Polymarket CLOB API": "clob.polymarket.com",
    "Polymarket Gamma API": "gamma-api.polymarket.com",
    "Binance WS": "stream.binance.com",
    "Alchemy RPC": "polygon-mainnet.g.alchemy.com",
}
for name, host in targets.items():
    try:
        start = time.perf_counter()
        socket.create_connection((host, 443), timeout=5)
        ms = (time.perf_counter() - start) * 1000
        print(f"  {name:30s} → {ms:6.1f} ms")
    except Exception as e:
        print(f"  {name:30s} → HIBA: {e}")

# 2. Polymarket API latency
print("\n[2] POLYMARKET CLOB API (HTTP GET)")
session = requests.Session()
latencies = []
for i in range(10):
    start = time.perf_counter()
    r = session.get("https://clob.polymarket.com/markets?limit=1", timeout=5)
    ms = (time.perf_counter() - start) * 1000
    latencies.append(ms)

s = sorted(latencies)
print(f"  Samples: {len(s)}")
print(f"  P50:     {s[len(s)//2]:.0f} ms")
print(f"  P95:     {s[int(len(s)*0.95)]:.0f} ms")
print(f"  Avg:     {statistics.mean(s):.0f} ms")
print(f"  Min:     {min(s):.0f} ms")
print(f"  Max:     {max(s):.0f} ms")

# 3. Polymarket Orderbook latency
print("\n[3] POLYMARKET ORDERBOOK (HTTP GET /book)")
ob_latencies = []
# Use a known active token
for i in range(10):
    start = time.perf_counter()
    r = session.get("https://clob.polymarket.com/books", timeout=5)
    ms = (time.perf_counter() - start) * 1000
    ob_latencies.append(ms)

s2 = sorted(ob_latencies)
print(f"  Samples: {len(s2)}")
print(f"  P50:     {s2[len(s2)//2]:.0f} ms")
print(f"  Avg:     {statistics.mean(s2):.0f} ms")
print(f"  Min:     {min(s2):.0f} ms")

# 4. Alchemy RPC latency
print("\n[4] ALCHEMY POLYGON RPC")
import os
from dotenv import load_dotenv
load_dotenv()
rpc_url = os.getenv("POLY_RPC_URL", "https://polygon-rpc.com")
rpc_latencies = []
for i in range(10):
    payload = {"jsonrpc": "2.0", "method": "eth_blockNumber", "params": [], "id": 1}
    start = time.perf_counter()
    r = session.post(rpc_url, json=payload, timeout=5)
    ms = (time.perf_counter() - start) * 1000
    rpc_latencies.append(ms)

s3 = sorted(rpc_latencies)
print(f"  Samples: {len(s3)}")
print(f"  P50:     {s3[len(s3)//2]:.0f} ms")
print(f"  Avg:     {statistics.mean(s3):.0f} ms")
print(f"  Min:     {min(s3):.0f} ms")

# Summary
print("\n" + "=" * 55)
print("  ÖSSZESÍTÉS")
print("=" * 55)
targets_met = {
    "Polymarket CLOB": (statistics.mean(latencies), 50, "< 50ms"),
    "Alchemy RPC": (statistics.mean(rpc_latencies), 100, "< 100ms"),
}
for name, (avg, target, label) in targets_met.items():
    status = "✅ PASS" if avg < target else "⚠️  SLOW"
    print(f"  {name:25s} Avg: {avg:5.0f}ms  Target: {label:10s} {status}")

print("\nSPEED_TEST_DONE")
