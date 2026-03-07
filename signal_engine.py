# pyre-ignore-all-errors
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
        self.thread: Optional[threading.Thread] = None
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
        
        # Spike detektáláshoz csak az első 10 bid-et nézzük (gyorsabb, nem blokkol)
        bids = data.get('bids', [])
        count = 0
        for bid in bids:
            if count >= 10: break
            count += 1
            try:
                size = float(bid.get('size', 0))
                if size > 100:
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
        self.last_signal_time: float = 0.0
        # Árfolyam történet (ms, price)
        self.price_history: Deque[Tuple[float, float]] = deque(maxlen=500)
        # OBI történet (ms, imbalance_ratio) - 5 sec window ~ 200 ticks
        self.obi_history: Deque[Tuple[float, float]] = deque(maxlen=200)
        self._last_obi_bias: float = 0.0

        # --- Paraméterek betöltése a strategy_maker.json-ból ---
        _sig_cfg = (
            config._config
            .get("order_management", {})
            .get("entry", {})
            .get("signal_params", {})
        )
        self.returns_window_ms: float = float(_sig_cfg.get("returns_window_ms", 120_000))
        self.z_threshold: float = float(_sig_cfg.get("z_threshold", 3.5))
        self.min_time_between_signals_ms: float = float(_sig_cfg.get("min_time_between_signals_ms", 8_000))
        self.imbalance_weight: float = float(_sig_cfg.get("imbalance_weight", 1.0)) # OBI rásegítő
        # --------------------------------------------------------

        # (timestamp_ms, r_t) párok
        self.returns: Deque[Tuple[float, float]] = deque(maxlen=2400)  # 2x buffer for 2min window
        self.Z_WARMUP = 150  # Min. adatpont a stabil szóráshoz

        # EWMV – Exponentially Weighted Moving Variance
        # α = 0.01: ~100 tick (~10s) felejtési sebesség – stabil viszonyítási alap
        self._ewm_alpha: float = 0.01
        self._ewm_mean: float = 0.0
        self._ewm_var: float = 0.0
        self._ewm_n: int = 0

        # Legacy paraméterek (fallback-nak megtartva)
        self.min_change_pct = 0.12
        self.max_change_pct = 0.20
        self.max_duration_ms = 1000

        logger.info(
            f"📡 SignalEngine init: z_threshold={self.z_threshold}, "
            f"debounce={self.min_time_between_signals_ms}ms, "
            f"window={self.returns_window_ms/1000:.0f}s"
        )

    
    def check_auto_halt(self) -> bool:
        # TODO: Hyperliquid L2 trade info based auto-halt logic can be placed here
        return False

    def update(self) -> Tuple[Optional[str], dict]:
        """
        Jel frissítés – hívd minden ~10-100ms-ban.

        Returns:
            (direction, metadata) tuple, ahol:
              direction: "BEARISH" | "BULLISH" | None (ha nincs jel)
              metadata:  {z_score, velocity_pct_sec, duration_ms, pct_change}
        """
        current_price = self.hl_feed.get_current_price()
        if not current_price:
            return None, {}

        now = time.time() * 1000  # milliseconds
        
        # OBI Kiolvasása az utolsó feed tickből (ha létezik)
        last_tick = self.hl_feed._last_tick
        current_imbalance = last_tick.imbalance if last_tick else 0.5

        r_t, z_t = self._update_returns_and_z(now, current_price, current_imbalance)

        # Debounce – túl sűrű jelek tiltása
        if now - self.last_signal_time < self.min_time_between_signals_ms:
            return None, {}

        def _build_meta(direction: str, r_t: float, z_t: float, duration_ms: float, sigma_r: float = 0.0) -> dict:
            velocity = abs(r_t * 1000.0 / max(duration_ms, 1)) * 100.0  # %/s becsült
            return {
                "z_score": float(f"{z_t:.4f}") if z_t is not None else 0.0,
                "obi_bias": float(f"{self._last_obi_bias:.3f}"),
                "velocity_pct_sec": float(f"{velocity:.5f}"),
                "duration_ms": float(f"{duration_ms:.1f}"),
                "pct_change": float(f"{r_t * 100.0:.4f}") if r_t is not None else 0.0,
                "direction": direction,
                "sigma_r": sigma_r
            }

        def _get_duration() -> Tuple[float, float, float]:
            if len(self.price_history) >= 2:
                ots, op = self.price_history[-2]
                nts, np_ = self.price_history[-1]
                return ots, op, nts - ots
            return now - 1000, current_price, 1.0

        # Szórást kivesszük
        sigma_r = math.sqrt(max(self._ewm_var, 1e-10))
        
        # 1) Z-SCORE TRIGGER
        if z_t is not None and z_t <= -self.z_threshold:
            self.last_signal_time = now
            _, _, dur = _get_duration()
            return "BEARISH", _build_meta("BEARISH", r_t or 0.0, z_t, dur, sigma_r)

        if z_t is not None and z_t >= self.z_threshold:
            self.last_signal_time = now
            _, _, dur = _get_duration()
            return "BULLISH", _build_meta("BULLISH", r_t or 0.0, z_t, dur, sigma_r)

        # 2) Fallback: fix %-os trigger (warm-up alatt nincs Z-score)
        move = self._detect_move(now, current_price)
        if move:
            self.last_signal_time = now
            return move.direction, {
                "z_score": 0.0,
                "velocity_pct_sec": abs(move.pct_change / max(move.duration_ms / 1000.0, 0.001)),
                "duration_ms": move.duration_ms,
                "pct_change": move.pct_change,
                "direction": move.direction,
            }

        return None, {}

    def _update_returns_and_z(
        self,
        now_ms: float,
        price: float,
        imbalance: float = 0.5
    ) -> Tuple[Optional[float], Optional[float]]:
        """
        Másodperces (ill. tick-közi) hozam és Z-score számítása
        az elmúlt returns_window_ms időablakra.
        """
        # Ha még nincs előző ár, csak inicializáljuk a history-t
        if not self.price_history:
            self.price_history.append((now_ms, price))
            self.obi_history.append((now_ms, imbalance))
            return None, None

        last_ts, last_price = self.price_history[-1]
        dt_ms = now_ms - last_ts
        if dt_ms <= 0 or last_price <= 0:
            # Degenerált adat – csak frissítjük az árfolyamot
            self.price_history.append((now_ms, price))
            self.obi_history.append((now_ms, imbalance))
            return None, None

        # Diszkrét hozam
        r_t = (price - last_price) / last_price

        # Árfolyam history frissítése
        self.price_history.append((now_ms, price))
        
        # OBI (Imbalance) History frissítése és "smooth" OBI számítás (pl utolsó 5 másodperc)
        self.obi_history.append((now_ms, imbalance))
        obi_cutoff = now_ms - 5000  # 5 másodperces simítás
        while self.obi_history and self.obi_history[0][0] < obi_cutoff:
            self.obi_history.popleft()
            
        avg_obi = sum(o[1] for o in self.obi_history) / max(len(self.obi_history), 1)

        # Hozamtörténet bővítése és ablak tisztítása
        self.returns.append((now_ms, r_t))
        cutoff = now_ms - self.returns_window_ms
        while self.returns and self.returns[0][0] < cutoff:
            self.returns.popleft()

        # EWMV – Exponentially Weighted Moving Variance (O(1), és FELEJT!)
        # Frissítés képlet:
        #   mean_t = (1 - α) * mean_{t-1} + α * r_t
        #   var_t  = (1 - α) * var_{t-1}  + α * (r_t - mean_{t-1})²
        # A négyzet ELŐTTI mean értékével számolunk (Welford-kompatibilis forma).
        alpha = self._ewm_alpha
        prev_mean = self._ewm_mean
        self._ewm_mean = (1 - alpha) * self._ewm_mean + alpha * r_t
        self._ewm_var  = (1 - alpha) * self._ewm_var  + alpha * (r_t - prev_mean) ** 2
        self._ewm_n   += 1

        # Warm-up: legalább Z_WARMUP tick kell a stabil becsléshez
        if self._ewm_n < self.Z_WARMUP:
            return r_t, None

        sigma_r = math.sqrt(max(self._ewm_var, 1e-10))  # epsilon=1e-10 (mentor formula)
        if sigma_r <= 0:
            return r_t, None

        base_z_t = (r_t - self._ewm_mean) / sigma_r
        
        # --- OBI Bias Beépítése ---
        # Ha az avg_obi > 0.5, akkor a könyv "long-ra áll". Ezt hozzáadjuk a Z-scorehoz,
        # hogy egy kisebb kiugrás is elég legyen a triggerhez (felgyorsítva a belépést).
        # Normalizáljuk az [0.0 - 1.0] OBI-t [-1.0 - +1.0] skálára.
        obi_normalized = (avg_obi - 0.5) * 2.0 
        
        # Bias kiszámítása az elvárt súllyal. Pl imbalance_weight = 1.0 esetén max +/- 1.0 Z-score módosítást ad.
        obi_bias = obi_normalized * self.imbalance_weight
        self._last_obi_bias = obi_bias # Eltároljuk a metadatákhoz
        
        final_z_t = base_z_t + obi_bias
        
        return r_t, final_z_t
    
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
        Toxikus flow detektor.
        Logika: ha a mozdulat TARTÓS (duration) ÉS GYORS (velocity),
        akkor informált kereskedő mozoghat – ne lépjünk be.

        Threshold-ok (mentor javaslatai alapján):
          velocity > 0.25%/s  →  gyors mozgás
          duration > 300ms    →  tartós mozgás (nem flash-zajj)
        """
        VELOCITY_THRESHOLD = 0.25  # %/sec
        # 600ms: elkerüli a WebSocket batch-jitter miatti téves riasztást,
        # de még mindig időben reagál a valódi bálnamozgásokra.
        # (Mentor: 300ms túl agresszív, javasolt: 500-800ms)
        DURATION_THRESHOLD_MS = 600  # ms

        if velocity_pct_per_sec > VELOCITY_THRESHOLD and duration_ms > DURATION_THRESHOLD_MS:
            logger.warning(
                f"⚠️ TOXIC FLOW: velocity={velocity_pct_per_sec:.3f}%/s, "
                f"duration={duration_ms}ms – belépés kihagyva!"
            )
            return True
        return False


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
        direction, meta = engine.update()

        if direction:
            logger.info(f"🚨 SIGNAL DETECTED!")
            logger.info(f"   Direction: {direction}")
            logger.info(f"   Z-score:   {meta.get('z_score', 0):.4f}")
            logger.info(f"   Change:    {meta.get('pct_change', 0):+.3f}%")
            logger.info(f"   Duration:  {meta.get('duration_ms', 0):.0f}ms")
            logger.info(f"   Velocity:  {meta.get('velocity_pct_sec', 0):.4f}%/s")

        time.sleep(0.1)  # 100ms

    hl.stop()
    logger.info("✅ Test complete")
