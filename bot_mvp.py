"""
Crypto Sebesseg - Minimal MVP Bot
Maker-only strategy with ambush ladder
"""
import time
import sys
from config import config
from binance_feed import BinanceFeed
from market_finder import MarketFinder
from polymarket_client import PolymarketClient
from fee_utils import fetch_fee_rate_bps

print("=" * 60)
print("🚀 CRYPTO SEBESSEG - MAKER BOT MVP")
print("=" * 60)
print()

# Configuration
print("📋 STRATEGY CONFIG:")
print(f"   Name: {config.strategy_name}")
print(f"   Type: {config.strategy_type}")
print(f"   DRY RUN: {config.dry_run}")
print()

# Initialize components
print("🔧 Initializing components...")

# 1. Binance Feed
print("  ├─ Binance WebSocket feed...", end=" ", flush=True)
binance = BinanceFeed()
binance.start()
time.sleep(2)  # Wait for connection
if binance.get_current_price():
    print("✅")
else:
    print("❌ Failed to connect to Binance")
    sys.exit(1)

# 2. Market Finder
print("  ├─ Polymarket market finder...", end=" ", flush=True)
finder = MarketFinder()
print("✅")

# 3. Polymarket Client
print("  └─ Polymarket CLOB client...", end=" ", flush=True)
poly_client = PolymarketClient()
if poly_client.initialize():
    print("✅")
else:
    print("❌ Failed to initialize Polymarket client")
    print("     Check your PRIVATE_KEY in .env")
    sys.exit(1)

print()
print("=" * 60)
print("✅ ALL COMPONENTS READY")
print("=" * 60)
print()

# Find active market
print("🔍 Finding active 15-min BTC UP/DOWN market...")
try:
    market_ctx = finder.get_active_market(binance)
    
    if market_ctx:
        print(f"✅ MARKET FOUND:")
        print(f"   Slug: {market_ctx.slug}")
        print(f"   Market ID: {market_ctx.market_id}")
        print(f"   Asset: {market_ctx.asset}")
        print(f"   Start: {market_ctx.start}")
        print(f"   End: {market_ctx.end}")
        print(f"   S0 Truth: ${market_ctx.s0_truth:,.2f}")
        print(f"   UP Token: {market_ctx.up_token_id[:20]}...")
        print(f"   DOWN Token: {market_ctx.down_token_id[:20]}...")
        
        # Calculate time to resolution
        import datetime
        now = datetime.datetime.now(datetime.timezone.utc)
        time_to_res = (market_ctx.end - now).total_seconds()
        print(f"   Time to resolution: {int(time_to_res)}s")
        
        # Check if within trading window
        min_ttr = config.min_time_to_resolution_sec
        max_ttr = config.max_time_to_resolution_sec
        
        if time_to_res < min_ttr:
            print(f"\n⚠️  Market too close to resolution ({time_to_res}s < {min_ttr}s)")
            print("   Waiting for next market...")
        elif time_to_res > max_ttr:
            print(f"\n⚠️  Market too far from resolution ({time_to_res}s > {max_ttr}s)")
            print("   Waiting for next market...")
        else:
            print(f"\n✅ Market is in trading window ({min_ttr}s - {max_ttr}s)")
            
            # Get current BTC price
            btc_price = binance.get_current_price()
            print(f"\n📊 CURRENT STATE:")
            print(f"   BTC Price: ${btc_price:,.2f}")
            print(f"   S0 (start): ${market_ctx.s0_truth:,.2f}")
            print(f"   Delta: ${btc_price - market_ctx.s0_truth:+,.2f} ({((btc_price / market_ctx.s0_truth - 1) * 100):+.3f}%)")
            
            # Fetch orderbook forUP and DOWN
            print(f"\n📖 ORDERBOOK:")
            try:
                up_ask = poly_client.get_ask_price(market_ctx.up_token_id)
                down_ask = poly_client.get_ask_price(market_ctx.down_token_id)
                
                if up_ask and down_ask:
                    print(f"   UP Ask: ${up_ask:.2f}")
                    print(f"   DOWN Ask: ${down_ask:.2f}")
                    
                    # Calculate mid and spread
                    # Note: For simplicity, assuming bid ≈ ask - 0.02
                    up_bid = max(0.01, up_ask - 0.02)
                    down_bid = max(0.01, down_ask - 0.02)
                    
                    up_mid = (up_bid + up_ask) / 2
                    down_mid = (down_bid + down_ask) / 2
                    
                    up_spread = up_ask - up_bid
                    down_spread = down_ask - down_bid
                    
                    print(f"   UP Mid: ${up_mid:.2f} (spread: ${up_spread:.2f})")
                    print(f"   DOWN Mid: ${down_mid:.2f} (spread: ${down_spread:.2f})")
                    
                    # Check  if spread is acceptable
                    if up_spread > config.max_spread or down_spread > config.max_spread:
                        print(f"\n⚠️  Spread too wide (max allowed: ${config.max_spread:.2f})")
                    else:
                        print(f"\n✅ Spread acceptable")
                        
                        # Fetch fee rates
                        print(f"\n💰 FEE RATES:")
                        try:
                            up_fee = fetch_fee_rate_bps(market_ctx.up_token_id)
                            down_fee = fetch_fee_rate_bps(market_ctx.down_token_id)
                            print(f"   UP Token: {up_fee} bps ({up_fee/100}%)")
                            print(f"   DOWN Token: {down_fee} bps ({down_fee/100}%)")
                        except Exception as e:
                            print(f"   ⚠️  Could not fetch fees: {e}")
                        
                        print(f"\n" + "=" * 60)
                        print("🎯 BOT IS READY TO TRADE")
                        print("=" * 60)
                        print()
                        print(f"💡 NEXT STEPS:")
                        print(f"   1. Monitor Binance for triggers (0.12-0.20% moves < 1s)")
                        print(f"   2. On trigger → place 3-level post-only ladder")
                        print(f"   3. Wait 1.5s for fills")
                        print(f"   4. If filled → place TP + time-stop")
                        print()
                        print(f"⚠️  Currently in MVP mode - signal engine not yet implemented")
                        print(f"   Set DRY_RUN=false in .env for live trading (when ready)")
                        
                else:
                    print(f"   ⚠️  Could not fetch orderbook")
                    
            except Exception as e:
                print(f"   ❌ Orderbook error: {e}")
    else:
        print("❌ No active market found")
        print("   - Check that 15-min BTC UP/DOWN markets are active")
        print("   - Markets typically run during US hours")

except Exception as e:
    print(f"❌ Market finder error: {e}")
    import traceback
    traceback.print_exc()

# Cleanup
print()
print("🛑 Shutting down...")
binance.stop()
print("✅ Done")
