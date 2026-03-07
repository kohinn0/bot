import asyncio
import time
from bot_logger import logger
from hyperliquid_feed import HyperliquidFeed
from signal_engine import SignalEngine

async def main():
    logger.info("Starting OBI Test...")
    feed = HyperliquidFeed(coin="BTC")
    engine = SignalEngine(feed)
    feed.start()
    
    try:
        # Várjuk meg, amíg a feed betölti az L2 order book-ot
        await asyncio.sleep(2)
        
        for _ in range(15):
            now = time.time() * 1000
            current_price = feed.get_current_price()
            if not current_price:
                await asyncio.sleep(0.5)
                continue
                
            last_tick = feed._last_tick
            obi = last_tick.imbalance if last_tick else 0.5
            
            # Kiszámoljuk a belső statisztikákat
            r_t, final_z = engine._update_returns_and_z(now, current_price, obi)
            
            # Kiírjuk
            bias = getattr(engine, "_last_obi_bias", 0.0)
            avg_obi = sum(o[1] for o in engine.obi_history) / max(len(engine.obi_history), 1) if hasattr(engine, "obi_history") else 0.5
            
            print(f"[{time.strftime('%H:%M:%S')}] Price: {current_price} | Tick OBI: {obi:.3f} | Avg 5s OBI: {avg_obi:.3f} | Z-Bias: {bias:.3f} | Base Z: {(final_z - bias) if final_z else 0:.3f} | Final Z: {final_z if final_z else 0:.3f}")
            await asyncio.sleep(1)
            
    finally:
        feed.stop()

if __name__ == "__main__":
    asyncio.run(main())
