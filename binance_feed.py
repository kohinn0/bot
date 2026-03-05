from bot_logger import logger
"""
Binance Feed v3 - Ultra Low Latency WebSocket
Optimalizációk:
- websockets + asyncio (nem blocking)
- TCP_NODELAY
- NTP offset kompenzáció
- Külön thread, publish pattern
"""
import math
import asyncio
import threading
import time
import json
import socket
from collections import deque
from dataclasses import dataclass
from typing import Deque, Optional, Callable
from queue import Queue, Empty

import requests
import websockets

# Compatibility with new config
BINANCE_REST_URL = "https://api.binance.com"
BINANCE_SYMBOL = "BTCUSDT"
VOL_WINDOW_SEC = 60



@dataclass(frozen=True)
class PricePoint:
    t: float  # local timestamp
    p: float  # price


@dataclass
class TickEvent:
    """Egy tick esemény - azonnal publikálva"""
    local_time_ms: float
    event_time_ms: float
    price: float
    latency_ms: float


class BinanceFeed:
    """
    Ultra-low latency Binance WebSocket feed.
    - Asyncio WS kliens külön thread-ben
    - NTP offset kompenzáció
    - Publish pattern a fő thread-hez
    """
    
    WS_URL = f"wss://stream.binance.com:9443/ws/{BINANCE_SYMBOL.lower()}@trade"
    
    def __init__(self, session: Optional[requests.Session] = None):
        self.session = session or requests.Session()
        self._buf: Deque[PricePoint] = deque()
        self._lock = threading.Lock()
        self._stop = threading.Event()
        
        # Current state
        self._current_price: Optional[float] = None
        self._last_update: float = 0
        self._last_tick: Optional[TickEvent] = None
        
        # NTP offset (local - server time)
        self._ntp_offset_ms: float = 0
        
        # Async event loop thread
        self._loop: Optional[asyncio.AbstractEventLoop] = None
        self._thread: Optional[threading.Thread] = None
        
        # Tick queue for external consumers
        self._tick_queue: Queue[TickEvent] = Queue(maxsize=1000)
        
    def start(self) -> None:
        """Indítja a WS feed-et külön thread-ben"""
        if self._thread and self._thread.is_alive():
            return
        
        self._stop.clear()
        
        # NTP offset mérés
        self._measure_ntp_offset()
        
        # Asyncio loop külön thread-ben
        self._thread = threading.Thread(target=self._run_async_loop, daemon=True)
        self._thread.start()
        
        logger.info(f"✅ Binance Feed v3 Started (NTP offset: {self._ntp_offset_ms:.0f}ms)")
    
    def stop(self) -> None:
        """Leállítás"""
        self._stop.set()
        if self._loop:
            self._loop.call_soon_threadsafe(self._loop.stop)
        if self._thread:
            self._thread.join(timeout=2)
        logger.info("🛑 Binance Feed Stopped")
    
    def _measure_ntp_offset(self) -> None:
        """
        Méri a local clock vs Binance server time különbséget.
        Ez kritikus a valós latency számításhoz!
        """
        try:
            # Binance server time endpoint
            t1 = time.time() * 1000
            r = self.session.get(f"{BINANCE_REST_URL}/api/v3/time", timeout=2)
            t2 = time.time() * 1000
            
            if r.status_code == 200:
                server_time = r.json()["serverTime"]
                # RTT / 2 kompenzáció
                local_time = (t1 + t2) / 2
                self._ntp_offset_ms = local_time - server_time
                logger.info(f"   📡 NTP offset: {self._ntp_offset_ms:.0f}ms (local - server)")
        except Exception as e:
            logger.info(f"   ⚠️ NTP mérés hiba: {e}")
            self._ntp_offset_ms = 0
    
    def _run_async_loop(self) -> None:
        """Asyncio event loop futtatása külön thread-ben"""
        self._loop = asyncio.new_event_loop()
        asyncio.set_event_loop(self._loop)
        
        try:
            self._loop.run_until_complete(self._ws_handler())
        except Exception as e:
            if not self._stop.is_set():
                logger.info(f"⚠️ Async loop error: {e}")
        finally:
            self._loop.close()
    
    async def _ws_handler(self) -> None:
        """WebSocket kezelő - reconnect loop-pal"""
        while not self._stop.is_set():
            try:
                await self._connect_and_listen()
            except Exception as e:
                if not self._stop.is_set():
                    logger.info(f"⚠️ WS Error: {e}, reconnecting in 1s...")
                    await asyncio.sleep(1)
    
    async def _connect_and_listen(self) -> None:
        """Kapcsolódás és hallgatás"""
        
        # Simple websocket connection
        async with websockets.connect(
            self.WS_URL,
            ping_interval=20,
            ping_timeout=10,
            close_timeout=5
        ) as ws:
            logger.info("🔗 Binance WS Connected (optimized)")
            
            async for message in ws:
                if self._stop.is_set():
                    break
                    
                # Azonnal feldolgozás
                self._process_message(message)
    
    def _process_message(self, message: str) -> None:
        """
        Üzenet feldolgozása - minimális overhead!
        """
        local_time_ms = time.time() * 1000
        
        try:
            data = json.loads(message)
            
            event_time_ms = data.get("E") or data.get("T", 0)
            price = float(data.get("p", 0))
            
            if price <= 0:
                return
            
            # Valós latency (NTP kompenzált)
            # latency = local_receive - event_time - ntp_offset
            corrected_local = local_time_ms - self._ntp_offset_ms
            latency_ms = corrected_local - event_time_ms
            
            # Tick event létrehozása
            tick = TickEvent(
                local_time_ms=local_time_ms,
                event_time_ms=event_time_ms,
                price=price,
                latency_ms=latency_ms
            )
            
            # State frissítés (thread-safe)
            self._current_price = price
            self._last_update = local_time_ms / 1000
            self._last_tick = tick
            
            # Buffer frissítés
            with self._lock:
                self._buf.append(PricePoint(local_time_ms / 1000, price))
                self._trim(local_time_ms / 1000)
            
            # Publish to queue (non-blocking)
            try:
                self._tick_queue.put_nowait(tick)
            except Exception:
                pass  # Queue full, drop oldest
                
        except json.JSONDecodeError:
            pass  # Binary/ping frames - expected
        except Exception as e:
            # Log unexpected parse errors so format changes are visible
            import traceback
            logger.info(f"⚠️ BinanceFeed parse error: {e}")
    
    def _trim(self, now: float) -> None:
        """Buffer takarítás"""
        cutoff = now - (VOL_WINDOW_SEC + 5)
        while self._buf and self._buf[0].t < cutoff:
            self._buf.popleft()
    
    def get_current_price(self) -> Optional[float]:
        """Aktuális ár - azonnal visszatér"""
        if self._current_price and (time.time() - self._last_update) < 5:
            return self._current_price
        
        with self._lock:
            if self._buf:
                return self._buf[-1].p
        
        # Fallback REST
        try:
            return self.get_spot_price()
        except Exception:
            return None
    
    def get_last_tick(self) -> Optional[TickEvent]:
        """Utolsó tick esemény részletekkel"""
        return self._last_tick
    
    def get_latest_latency_ms(self) -> Optional[float]:
        """Aktuális WS latency (NTP kompenzált)"""
        if self._last_tick:
            return self._last_tick.latency_ms
        return None
    
    def get_spot_price(self) -> float:
        """REST fallback"""
        r = self.session.get(
            f"{BINANCE_REST_URL}/api/v3/ticker/price",
            params={"symbol": BINANCE_SYMBOL},
            timeout=2,
        )
        r.raise_for_status()
        return float(r.json()["price"])
    
    def get_sigma_per_s(self, window_sec: int = VOL_WINDOW_SEC) -> Optional[float]:
        """Volatilitás számítás"""
        with self._lock:
            if len(self._buf) < 5:
                return None
            now = self._buf[-1].t
            cutoff = now - window_sec
            pts = [pp for pp in self._buf if pp.t >= cutoff]

        if len(pts) < 5:
            return None

        rets = []
        dts = []
        for i in range(1, len(pts)):
            p0, p1 = pts[i - 1].p, pts[i].p
            t0, t1 = pts[i - 1].t, pts[i].t
            dt = t1 - t0
            if dt <= 0 or p0 <= 0 or p1 <= 0:
                continue
            r = math.log(p1 / p0)
            rets.append(r)
            dts.append(dt)

        if len(rets) < 3:
            return None

        num = sum(r * r for r in rets)
        den = sum(dts)
        if den <= 0:
            return None
        return math.sqrt(max(num / den, 0.0))

    def get_price_at_ms(self, timestamp_ms: int) -> Optional[float]:
        """Történelmi ár"""
        try:
            r = self.session.get(
                f"{BINANCE_REST_URL}/api/v3/aggTrades",
                params={"symbol": BINANCE_SYMBOL, "startTime": int(timestamp_ms), "limit": 1},
                timeout=3,
            )
            r.raise_for_status()
            data = r.json()
            if not data:
                return None
            return float(data[0]["p"])
        except Exception:
            return None

    @staticmethod
    def sd_price(current_price: float, sigma_per_s: float, time_left_s: float) -> float:
        """Áringadozás számítás"""
        if current_price <= 0 or sigma_per_s <= 0 or time_left_s <= 0:
            return 0.0
        return current_price * sigma_per_s * math.sqrt(time_left_s)


# ============================================================
# QUICK TEST
# ============================================================
if __name__ == "__main__":
    import statistics
    
    logger.info("🧪 Binance Feed v3 - Latency Test")
    logger.info("=" * 50)
    
    feed = BinanceFeed()
    feed.start()
    
    time.sleep(2)  # Warm up
    
    latencies = []
    logger.info("\n📊 Collecting 100 ticks...")
    
    for i in range(100):
        tick = feed.get_last_tick()
        if tick:
            latencies.append(tick.latency_ms)
        time.sleep(0.1)
    
    feed.stop()
    
    if latencies:
        sorted_lat = sorted(latencies)
        p50 = sorted_lat[len(sorted_lat) // 2]
        p95 = sorted_lat[int(len(sorted_lat) * 0.95)]
        avg = statistics.mean(latencies)
        
        logger.info(f"\n📈 RESULTS (NTP kompenzált):")
        logger.info(f"   Samples: {len(latencies)}")
        logger.info(f"   P50: {p50:.0f}ms | P95: {p95:.0f}ms | Avg: {avg:.0f}ms")
