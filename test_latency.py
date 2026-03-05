"""Quick Binance latency test from VPS"""
from binance_feed import BinanceFeed
import time, statistics

print("BINANCE LATENCY TEST")
print("=" * 40)

feed = BinanceFeed()
feed.start()
time.sleep(3)

latencies = []
for i in range(50):
    tick = feed.get_last_tick()
    if tick:
        latencies.append(tick.latency_ms)
    time.sleep(0.1)

feed.stop()

if latencies:
    s = sorted(latencies)
    print(f"Samples: {len(latencies)}")
    print(f"P50: {s[len(s)//2]:.0f}ms")
    print(f"P95: {s[int(len(s)*0.95)]:.0f}ms")
    print(f"Avg: {statistics.mean(latencies):.0f}ms")
    print("LATENCY_TEST_OK")
else:
    print("NO DATA - check connection")
