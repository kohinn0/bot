from bot_logger import logger
import asyncio
import json
import math
import time
import threading
from collections import deque
from typing import Deque, Optional, Tuple
from dataclasses import dataclass

import websockets

from hyperliquid_feed import HyperliquidFeed
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
            logger.info(f"⚠️ Polymarket WS hiba: {e}")

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
             logger.info(f"🚨🚨 VOLUMEN SPIKE DETEKTÁLVA! ({total_vol} shares/sec) 🚨🚨")
             self.halt_signal = True


class SignalEngine:
    """Detects momentum signals from Hyperliquid L2 feed"""
    
    def __init__(self, hl_feed: HyperliquidFeed):
        self.hl_feed = hl_feed
        self.last_signal_time = 0
        # Árfolyam történet (ms, price)
        self.price_history: Deque[Tuple[float, float]] = deque(maxlen=500)
        
        # Dinamikus Z-score alapú triggerhez
        # Az elmúlt ~60s hozamait tartjuk egy mozgó ablakban
        self.returns_window_ms = 60_000
        # (timestamp_ms, r_t) párok
        self.returns: Deque[Tuple[float, float]] = deque(maxlen=1200)
        # Z threshold (tipikusan 2.5–3.0 → 3σ anomália)
        self.z_threshold = 2.5

        # Legacy paraméterek (fallback-nak megtartva)
        self.min_change_pct = 0.12  # 0.12%
        self.max_change_pct = 0.20  # 0.20%
        self.max_duration_ms = 1000  # 1 second
        self.min_time_between_signals_ms = 3000  # 3 seconds
    
    def check_auto_halt(self) -> bool:
        # TODO: Hyperliquid L2 trade info based auto-halt logic can be placed here
        return False

    def update(self) -> Optional[PriceMove]:
        """
        Check for signals. Call this frequently (every ~100ms).
        
        Returns:
            PriceMove if signal detected, None otherwise
        """
        current_price = self.hl_feed.get_current_price()
        if not current_price:
            return None
        
        now = time.time() * 1000  # milliseconds

        # Frissítjük az árfolyam- és hozamtörténetet, kiszámoljuk az aktuális Z-score-t
        r_t, z_t = self._update_returns_and_z(now, current_price)
        
        # Debounce – túl sűrű jelek tiltása
        if now - self.last_signal_time < self.min_time_between_signals_ms:
            return None
        
        # 1) DINAMIKUS Z-SCORE TRIGGER
        # Bearish: Z_t < -k  (anomális esés)
        if z_t is not None and z_t <= -self.z_threshold:
            self.last_signal_time = now
            pct_change = (r_t or 0.0) * 100.0
            # A legutóbbi két ár alapján becsült start/end
            if len(self.price_history) >= 2:
                oldest_ts, oldest_price = self.price_history[-2]
                newest_ts, newest_price = self.price_history[-1]
            else:
                oldest_ts, oldest_price = now - 1000, current_price
                newest_ts, newest_price = now, current_price
            duration_ms = newest_ts - oldest_ts
            return PriceMove(
                direction="BEARISH",
                start_price=oldest_price,
                end_price=newest_price,
                pct_change=pct_change,
                duration_ms=duration_ms,
                timestamp=now,
            )

        # Bullish: Z_t > +k  (anomális emelkedés)
        if z_t is not None and z_t >= self.z_threshold:
            self.last_signal_time = now
            pct_change = (r_t or 0.0) * 100.0
            if len(self.price_history) >= 2:
                oldest_ts, oldest_price = self.price_history[-2]
                newest_ts, newest_price = self.price_history[-1]
            else:
                oldest_ts, oldest_price = now - 1000, current_price
                newest_ts, newest_price = now, current_price
            duration_ms = newest_ts - oldest_ts
            return PriceMove(
                direction="BULLISH",
                start_price=oldest_price,
                end_price=newest_price,
                pct_change=pct_change,
                duration_ms=duration_ms,
                timestamp=now,
            )
        
        # 2) Fallback: régi, fix %-os trigger (ha még nincs elég adat a Z-score-hoz)
        signal = self._detect_move(now, current_price)
        
        if signal:
            self.last_signal_time = now
        
        return signal

    def _update_returns_and_z(
        self,
        now_ms: float,
        price: float,
    ) -> Tuple[Optional[float], Optional[float]]:
        """
        Másodperces (ill. tick-közi) hozam és Z-score számítása
        az elmúlt returns_window_ms időablakra.
        """
        # Ha még nincs előző ár, csak inicializáljuk a history-t
        if not self.price_history:
            self.price_history.append((now_ms, price))
            return None, None

        last_ts, last_price = self.price_history[-1]
        dt_ms = now_ms - last_ts
        if dt_ms <= 0 or last_price <= 0:
            # Degenerált adat – csak frissítjük az árfolyamot
            self.price_history.append((now_ms, price))
            return None, None

        # Diszkrét hozam (nem log-return, hogy illeszkedjen a korábbi dokumentumhoz)
        r_t = (price - last_price) / last_price

        # Árfolyam history frissítése
        self.price_history.append((now_ms, price))

        # Hozamtörténet bővítése és ablak tisztítása
        self.returns.append((now_ms, r_t))
        cutoff = now_ms - self.returns_window_ms
        while self.returns and self.returns[0][0] < cutoff:
            self.returns.popleft()

        # Ha még kevés adatunk van, nem számítunk Z-score-t
        if len(self.returns) < 20:
            return r_t, None

        # Z-score: Z_t = (r_t - mu_r) / sigma_r
        values = [x[1] for x in self.returns]
        mu_r = sum(values) / len(values)
        var = sum((x - mu_r) ** 2 for x in values) / max(len(values) - 1, 1)
        sigma_r = math.sqrt(max(var, 1e-18))

        if sigma_r <= 0:
            return r_t, None

        z_t = (r_t - mu_r) / sigma_r
        return r_t, z_t
    
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
    logger.info("🧪 Testing Signal Engine")
    logger.info("=" * 60)
    
    # Initialize
    hl = HyperliquidFeed()
    hl.start()
    time.sleep(2)
    
    engine = SignalEngine(hl)
    
    logger.info(f"Monitoring for signals...")
    logger.info(f"  Trigger range: {config.bearish_trigger_pct_range[0]:.2f}% - {config.bearish_trigger_pct_range[1]:.2f}%")
    logger.info(f"  Max duration: 1000ms")
    logger.info(f"  Debounce: {config.min_time_between_entries_ms}ms")

    # Monitor for 30 seconds
    for i in range(300):  # 30 seconds at 100ms intervals
        signal = engine.update()
        
        if signal:
            logger.info(f"🚨 SIGNAL DETECTED!")
            logger.info(f"   Direction: {signal.direction}")
            logger.info(f"   Change: {signal.pct_change:+.3f}%")
            logger.info(f"   Duration: {signal.duration_ms:.0f}ms")
            logger.info(f"   Price: ${signal.start_price:,.2f} → ${signal.end_price:,.2f}")

        time.sleep(0.1)  # 100ms
    
    hl.stop()
    logger.info("✅ Test complete")
