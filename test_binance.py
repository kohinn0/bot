"""
Crypto Sebesseg - Minimal Test (Binance Only)
Quick test to verify Binance connection
"""
import time
from binance_feed import BinanceFeed

print("🧪 TESTING BINANCE FEED")
print("=" * 60)

# Initialize Binance feed
print("Connecting to Binance WebSocket...")
binance = BinanceFeed()
binance.start()

# Wait for connection
time.sleep(3)

# Get current price
price = binance.get_current_price()
if price:
    print(f"✅ Connected!")
    print(f"   BTC Price: ${price:,.2f}")
    
    # Get latency
    latency = binance.get_latest_latency_ms()
    if latency:
        print(f"   Latency: {latency:.0f}ms")
    
    # Monitor for 10 seconds
    print(f"\n📊 Monitoring price for 10 seconds...")
    for i in range(10):
        time.sleep(1)
        new_price = binance.get_current_price()
        tick = binance.get_last_tick()
        if tick:
            print(f"   [{i+1}/10] ${new_price:,.2f} (latency: {tick.latency_ms:.0f}ms)")
    
    print(f"\n✅ Binance feed working perfectly!")
else:
    print("❌ Failed to get price")

# Cleanup
binance.stop()
print("\n✅ Test complete")
