"""
Signal Proximity Monitor - Shows how close to signal trigger
"""
import time
from binance_feed import BinanceFeed
from collections import deque

print("🎯 SIGNAL PROXIMITY MONITOR")
print("=" * 70)
print("Threshold: 0.12% move in < 1 second")
print()

# Initialize
binance = BinanceFeed()
binance.start()
time.sleep(2)

# Track prices
price_window = deque(maxlen=10)  # Last 1 second (100ms samples)

try:
    while True:
        now = time.time() * 1000
        price = binance.get_current_price()
        
        if price:
            price_window.append((now, price))
            
            # Calculate current volatility
            if len(price_window) >= 2:
                oldest_ts, oldest_price = price_window[0]
                newest_ts, newest_price = price_window[-1]
                
                pct_change = ((newest_price - oldest_price) / oldest_price) * 100
                duration_ms = newest_ts - oldest_ts
                
                # Distance to trigger (0.12%)
                trigger_threshold = 0.12
                distance_to_trigger = abs(abs(pct_change) - trigger_threshold)
                proximity_pct = (abs(pct_change) / trigger_threshold) * 100
                
                # Visual indicator
                bars = int(proximity_pct / 10)
                bar_visual = "█" * bars + "░" * (10 - bars)
                
                # Direction
                direction = "📈 UP" if pct_change > 0 else "📉 DOWN"
                
                # Status
                if abs(pct_change) >= trigger_threshold:
                    status = "🚨 SIGNAL!"
                elif proximity_pct >= 80:
                    status = "⚠️  CLOSE!"
                elif proximity_pct >= 50:
                    status = "🟡 Warming"
                else:
                    status = "🟢 Calm"
                
                print(f"\r{direction} | ${price:,.2f} | "
                      f"{pct_change:+.3f}% | "
                      f"[{bar_visual}] {proximity_pct:3.0f}% | "
                      f"{status}    ", 
                      end="", flush=True)
            
        time.sleep(0.1)

except KeyboardInterrupt:
    print("\n\n🛑 Stopped")
    binance.stop()
