import asyncio
import json
import time
import threading
from collections import deque
from typing import Deque, Optional
from dataclasses import dataclass

import websockets

from binance_feed import BinanceFeed
from config import config


@dataclass
class PriceMove:
    """Detected price movement"""
    direction: str  # "BEARISH" or "BULLISH"
    start_price: float
    end_price: float
    pct_change: float
    duration_ms: float
    timestamp: float


class PolymarketWS:
    """Monitors Polymarket CLOB WebSocket for Auto-Halt signals"""
    def __init__(self, token_ids: list[str]):
        self.token_ids = token_ids
        self.ws_url = "wss://ws-subscriptions-clob.polymarket.com/ws/market"
        self.is_running = False
        self.thread = None
        self.halt_signal = False
        
        # Volume tracking map {token_id: [(timestamp, size_added)]}
        self.recent_volume: dict[str, Deque] = {t: deque(maxlen=200) for t in token_ids}
        self.spike_threshold_shares = 5000  # Simplified threshold
        self.time_window_sec = 1.0

    def start(self):
        self.is_running = True
        self.thread = threading.Thread(target=self._run_loop, daemon=True)
        self.thread.start()
        
    def stop(self):
        self.is_running = False
        if self.thread:
            self.thread.join(timeout=1.0)
            
    def check_halt(self) -> bool:
        """Kiváltódott-e az Auto-Halt (Volume Spike)"""
        # Auto nullázódik az olvasás után a példában
        if self.halt_signal:
            self.halt_signal = False
            return True
        return False

    def _run_loop(self):
        asyncio.run(self._listen_ws())
        
    async def _listen_ws(self):
        try:
            async with websockets.connect(self.ws_url) as ws:
                # Subscription üzenet összeállítása
                assets = [{"type": "market", "market_id": t} for t in self.token_ids]
                sub_msg = {"assets": assets}
                await ws.send(json.dumps(sub_msg))
                
                while self.is_running:
                    msg = await ws.recv()
                    data = json.loads(msg)
                    self._process_message(data)
        except Exception as e:
            print(f"⚠️ Polymarket WS hiba: {e}")

    def _process_message(self, data: dict):
        if 'asset_id' not in data or 'bids' not in data:
            return
            
        token = data['asset_id']
        if token not in self.recent_volume:
            return
            
        now = time.time()
        
        # Leegyszerűsített volumen elemzés
        # A valóságban a delta frissítéseket kelleni nézni a price level-eken
        for bid in data.get('bids', []):
            try:
                size = float(bid.get('size', 0))
                if size > 100:  # Csak a nagyobb ordereket nézzük a spike-hoz
                    self.recent_volume[token].append((now, size))
            except:
                pass
                
        # Spike detektálása
        self._detect_spike(token, now)
        
    def _detect_spike(self, token: str, now: float):
        cutoff = now - self.time_window_sec
        # Tisztítás
        while self.recent_volume[token] and self.recent_volume[token][0][0] < cutoff:
            self.recent_volume[token].popleft()
            
        total_vol = sum(size for _, size in self.recent_volume[token])
        if total_vol > self.spike_threshold_shares:
             print(f"🚨🚨 VOLUMEN SPIKE DETEKTÁLVA! ({total_vol} shares/sec) 🚨🚨")
             self.halt_signal = True


class SignalEngine:
    """Detects momentum signals from Binance price feed and Auto-Halts from Polymarket"""
    
    def __init__(self, binance: BinanceFeed):
        self.binance = binance
        self.last_signal_time = 0
        self.price_history: Deque = deque(maxlen=100)
        self.poly_ws = None
        
        # Config
        self.min_change_pct = 0.12  # 0.12%
        self.max_change_pct = 0.20  # 0.20%
        self.max_duration_ms = 1000  # 1 second
        self.min_time_between_signals_ms = 3000  # 3 seconds
    
    def attach_polymarket_ws(self, token_ids: list[str]):
        """Csatlakoztatja az in-memory Polymarket WebSocket monitort a kért tokenekre"""
        if self.poly_ws:
            self.poly_ws.stop()
        self.poly_ws = PolymarketWS(token_ids)
        self.poly_ws.start()
        
    def check_auto_halt(self) -> bool:
        if self.poly_ws:
            return self.poly_ws.check_halt()
        return False

    def update(self) -> Optional[PriceMove]:
        """
        Check for signals. Call this frequently (every ~100ms).
        
        Returns:
            PriceMove if signal detected, None otherwise
        """
        current_price = self.binance.get_current_price()
        if not current_price:
            return None
        
        now = time.time() * 1000  # milliseconds
        
        # Add to history
        self.price_history.append((now, current_price))
        
        # Check debounce
        if now - self.last_signal_time < self.min_time_between_signals_ms:
            return None
        
        # Look for price moves in the last 1 second
        signal = self._detect_move(now, current_price)
        
        if signal:
            self.last_signal_time = now
        
        return signal
    
    def _detect_move(self, now: float, current_price: float) -> Optional[PriceMove]:
        """Detect significant price moves"""
        
        # Look back 1 second
        lookback_ms = self.max_duration_ms
        cutoff_time = now - lookback_ms
        
        # Find prices in the window
        prices_in_window = [
            (ts, price) for ts, price in self.price_history
            if ts >= cutoff_time
        ]
        
        if len(prices_in_window) < 2:
            return None
        
        # Get oldest and newest in window
        oldest_ts, oldest_price = prices_in_window[0]
        newest_ts, newest_price = prices_in_window[-1]
        
        # Calculate change
        pct_change = ((newest_price - oldest_price) / oldest_price) * 100
        duration_ms = newest_ts - oldest_ts
        
        # Check for bearish trigger (price drop)
        if pct_change <= -self.min_change_pct and pct_change >= -self.max_change_pct:
            return PriceMove(
                direction="BEARISH",
                start_price=oldest_price,
                end_price=newest_price,
                pct_change=pct_change,
                duration_ms=duration_ms,
                timestamp=now
            )
        
        # Check for bullish trigger (price rise)
        if pct_change >= self.min_change_pct and pct_change <= self.max_change_pct:
            return PriceMove(
                direction="BULLISH",
                start_price=oldest_price,
                end_price=newest_price,
                pct_change=pct_change,
                duration_ms=duration_ms,
                timestamp=now
            )
        
        return None
    
    def is_toxic_flow(self, velocity_pct_per_sec: float, duration_ms: int) -> bool:
        """
        Check if current flow is toxic (likely informed trading).
        
        Args:
            velocity_pct_per_sec: Price change rate (%/second)
            duration_ms: How long the move has been sustained
        
        Returns:
            True if flow is toxic and should be avoided
        """
        return config.is_toxic_flow(velocity_pct_per_sec, duration_ms)


if __name__ == "__main__":
    print("🧪 Testing Signal Engine")
    print("=" * 60)
    
    # Initialize
    binance = BinanceFeed()
    binance.start()
    time.sleep(2)
    
    engine = SignalEngine(binance)
    
    print(f"Monitoring for signals...")
    print(f"  Trigger range: {config.bearish_trigger_pct_range[0]:.2f}% - {config.bearish_trigger_pct_range[1]:.2f}%")
    print(f"  Max duration: 1000ms")
    print(f"  Debounce: {config.min_time_between_entries_ms}ms")
    print()
    
    # Monitor for 30 seconds
    for i in range(300):  # 30 seconds at 100ms intervals
        signal = engine.update()
        
        if signal:
            print(f"🚨 SIGNAL DETECTED!")
            print(f"   Direction: {signal.direction}")
            print(f"   Change: {signal.pct_change:+.3f}%")
            print(f"   Duration: {signal.duration_ms:.0f}ms")
            print(f"   Price: ${signal.start_price:,.2f} → ${signal.end_price:,.2f}")
            print()
        
        time.sleep(0.1)  # 100ms
    
    binance.stop()
    print("✅ Test complete")
