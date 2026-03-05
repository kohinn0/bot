# pyre-ignore-all-errors
from bot_logger import logger
import math
import asyncio
import threading
import time
import json
from collections import deque
from dataclasses import dataclass
from typing import Deque, Optional
from queue import Queue

import requests
import websockets

HL_WS_URL = "wss://api.hyperliquid.xyz/ws"
HL_REST_URL = "https://api.hyperliquid.xyz"
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


class HyperliquidFeed:
    """
    Ultra-low latency Hyperliquid WebSocket feed.
    Lecseréli a Binance feedet, közvetlenül a Hyperliquid /ws l2Book-ra csatlakozik.
    """
    
    def __init__(self, session: Optional[requests.Session] = None, coin: str = "BTC"):
        self.session = session or requests.Session()
        self.coin = coin
        self._buf: Deque[PricePoint] = deque()
        self._lock = threading.Lock()
        self._stop = threading.Event()
        
        # Állapotok
        self._current_price: Optional[float] = None
        self._last_update: float = 0
        self._last_tick: Optional[TickEvent] = None
        
        # Asyncio loop
        self._loop: Optional[asyncio.AbstractEventLoop] = None
        self._thread: Optional[threading.Thread] = None
        
        self._tick_queue: Queue[TickEvent] = Queue(maxsize=1000)
        
    def start(self) -> None:
        if self._thread and self._thread.is_alive():
            return
        
        self._stop.clear()
        
        # Asyncio loop külön thread-ben
        self._thread = threading.Thread(target=self._run_async_loop, daemon=True)
        self._thread.start()
        
        logger.info(f"✅ Hyperliquid Feed Started (Piac: {self.coin})")
    
    def stop(self) -> None:
        self._stop.set()
        if self._loop:
            self._loop.call_soon_threadsafe(self._loop.stop)
        if self._thread:
            self._thread.join(timeout=2)
        logger.info("🛑 Hyperliquid Feed Stopped")
    
    def _run_async_loop(self) -> None:
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
        while not self._stop.is_set():
            try:
                await self._connect_and_listen()
            except Exception as e:
                if not self._stop.is_set():
                    logger.info(f"⚠️ WS Error: {e}, reconnecting in 1s...")
                    await asyncio.sleep(1)
    
    async def _connect_and_listen(self) -> None:
        async with websockets.connect(
            HL_WS_URL,
            ping_interval=20,
            ping_timeout=10,
            close_timeout=5
        ) as ws:
            logger.info("🔗 Hyperliquid WS Connected (L2 Book stream)")
            
            sub_msg = {
                "method": "subscribe",
                "subscription": {"type": "l2Book", "coin": self.coin}
            }
            await ws.send(json.dumps(sub_msg))
            
            async for message in ws:
                if self._stop.is_set():
                    break
                self._process_message(message)
    
    def _process_message(self, message: str) -> None:
        local_time_ms = time.time() * 1000
        
        try:
            data = json.loads(message)
            channel = data.get("channel")
            if channel != "l2Book":
                return
                
            book_data = data.get("data", {})
            levels = book_data.get("levels", [])
            event_time_ms = book_data.get("time", local_time_ms)
            
            if len(levels) >= 2 and len(levels[0]) > 0 and len(levels[1]) > 0:
                best_bid = float(levels[0][0]["px"])
                best_ask = float(levels[1][0]["px"])
                mid_price = (best_bid + best_ask) / 2.0
                
                # Valós latency (NTP kompenzáció itt nincs a HL saját timestampjeit használjuk feltehetőleg)
                latency_ms = local_time_ms - event_time_ms if event_time_ms else 0
                
                tick = TickEvent(
                    local_time_ms=local_time_ms,
                    event_time_ms=event_time_ms,
                    price=mid_price,
                    latency_ms=latency_ms
                )
                
                # State frissítés
                self._current_price = mid_price
                self._last_update = local_time_ms / 1000
                self._last_tick = tick
                
                # Buffer frissítés a volatilitáshoz
                with self._lock:
                    self._buf.append(PricePoint(self._last_update, mid_price))
                    self._trim(self._last_update)
                
                try:
                    self._tick_queue.put_nowait(tick)
                except Exception:
                    pass
                
        except json.JSONDecodeError:
            pass
        except Exception as e:
            logger.info(f"⚠️ HyperliquidFeed parse error: {e}")
    
    def _trim(self, now: float) -> None:
        cutoff = now - (VOL_WINDOW_SEC + 5)
        while self._buf and self._buf[0].t < cutoff:
            self._buf.popleft()
    
    def get_current_price(self) -> Optional[float]:
        """Aktuális ár - azonnal visszatér (Mid Price L2-ből)"""
        if self._current_price and (time.time() - self._last_update) < 5:
            return self._current_price
        
        with self._lock:
            if self._buf:
                return self._buf[-1].p
        
        # Failover ha a WS nem adna semmit
        try:
            resp = self.session.post(HL_REST_URL + "/info", json={"type": "l2Book", "coin": self.coin}, timeout=2)
            if resp.status_code == 200:
                levels = resp.json().get("levels", [])
                if len(levels) == 2 and levels[0] and levels[1]:
                    return (float(levels[0][0]["px"]) + float(levels[1][0]["px"])) / 2.0
        except Exception:
            pass
        return None
    
    def get_staleness_sec(self) -> float:
        """Visszaadja a feed késésének másodpercét."""
        if not self._last_update:
            return 999.0
        return time.time() - self._last_update
    
    def is_feed_stale(self, max_staleness_sec: float = 3.0) -> bool:
        """Ellenőrzi, hogy a WebSocket adatok frissek-e."""
        return self.get_staleness_sec() > max_staleness_sec
    
    
    def get_last_tick(self) -> Optional[TickEvent]:
        return self._last_tick
    
    def get_sigma_per_s(self, window_sec: int = VOL_WINDOW_SEC) -> Optional[float]:
        """Volatilitás számítás ugyanúgy ahogy a Binance feed-ben"""
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

if __name__ == "__main__":
    logger.info("🧪 Hyperliquid WS Test")
    feed = HyperliquidFeed()
    feed.start()
    time.sleep(1)
    for _ in range(10):
        tick = feed.get_last_tick()
        if tick:
            logger.info(f"HL Price: ${tick.price} (Latency: {tick.latency_ms:.0f}ms)")
        time.sleep(1)
    feed.stop()
