import time
import os
import csv
from typing import Dict, List, Optional
from collections import deque
from dataclasses import dataclass

@dataclass
class TradeRecord:
    token_id: str
    side: str  # "UP" or "DOWN" (a vétel iránya)
    fill_price: float
    fill_size: float
    t_signal: float  # perf_counter origin
    t_quote_sent: float  # perf_counter origin
    wall_clock_time: float # time.time() origin for CSV log
    t_fill_perf: float # exact elapsed latency origin
    t_fill: float # time.time() origin for loop checks
    # Markout értékek rögzítéséhez:
    markout_250ms: Optional[float] = None
    markout_1s: Optional[float] = None
    markout_5s: Optional[float] = None

class ToxicityEngine:
    """
    Figyeli a kifizetések utóéletét (post-fill markout) és az adverse selection
    metrikákat. Egy egyszerű CSV fájlba naplózza az eredményeket az utólagos
    kvantitatív elemzéshez.
    """
    def __init__(self, log_dir: str = "logs"):
        self.pending_markouts: List[TradeRecord] = []
        self.toxicity_score: float = 0.0  # 0.0 (tiszta) -> 1.0 (mérgező)
        self.recent_markouts: deque = deque(maxlen=50)
        
        # Telemetria beállítás
        self.log_dir = log_dir
        if not os.path.exists(log_dir):
            os.makedirs(log_dir)
        
        self.log_file = os.path.join(log_dir, f"toxicity_log_{int(time.time())}.csv")
        self._init_csv()

    def _init_csv(self):
        """Inicializálja a CSV log fejlécét."""
        with open(self.log_file, "w", newline="") as f:
            writer = csv.writer(f)
            writer.writerow([
                "timestamp", "token_id", "side", "fill_price", "size",
                "latency_signal_to_quote_ms", "latency_quote_to_fill_ms",
                "markout_250ms_micro", "markout_1s_micro", "markout_5s_micro"
            ])

    def register_fill(self, token_id: str, side: str, price: float, size: float, t_signal: float, t_quote_sent: float, wall_clock_time: float):
        """Új kitöltés (fill) regisztrálása a markout figyeléshez."""
        record = TradeRecord(
            token_id=token_id,
            side=side,
            fill_price=price,
            fill_size=size,
            t_signal=t_signal,
            t_quote_sent=t_quote_sent,
            wall_clock_time=wall_clock_time,
            t_fill_perf=time.perf_counter(),
            t_fill=time.time()
        )
        self.pending_markouts.append(record)
        print(f"📊 [Toxicity] Fill regisztrálva: {side} @ ${price:.3f}. Markout mérés indítása...")

    def update_markouts(self, current_mid_prices: Dict[str, float]):
        """
        Periodikusan (tick-enként) meghívandó, hogy frissítse a várakozó trade-ek
        markout értékeit az eltelt idő függvényében.
        current_mid_prices: {token_id: current_mid_price}
        """
        now = time.time()
        active_records = []

        for record in self.pending_markouts:
            token = record.token_id
            if token not in current_mid_prices:
                active_records.append(record)
                continue
            
            # {token: {'mid': 0.5, 'microprice': 0.51}} formátum érkezik a bottól
            price_data = current_mid_prices[token]
            # Használjuk a volume-weighted microprice-t (ami spoofing-ellenállóbb és a vétel valós sodródását mutatja)
            current_eval_price = price_data.get('microprice', price_data.get('mid'))
            
            elapsed = now - record.t_fill

            # Számolás logikája: ha a microprice elmegy (csökken) a vételi oldal (YES) után,
            # az negatív markout. Ha DOWN-t vettünk (short ekvivalens), akkor a microprice növekedése = negatív markout.
            # Feltételezzük, hogy current_mid_prices mindig a YES token árbecslését adja vissza.
            if record.side == "UP":
                current_markout = current_eval_price - record.fill_price
            else:
                # DOWN token esetén fordított
                current_markout = record.fill_price - current_eval_price

            if elapsed >= 0.250 and record.markout_250ms is None:
                record.markout_250ms = current_markout
            
            if elapsed >= 1.0 and record.markout_1s is None:
                record.markout_1s = current_markout
                self.recent_markouts.append(current_markout)
                self._recalculate_toxicity()
                
            if elapsed >= 5.0 and record.markout_5s is None:
                record.markout_5s = current_markout
                self._log_record(record)
                # 5s után készen vagyunk vele
                continue

            active_records.append(record)

        self.pending_markouts = active_records

    def _recalculate_toxicity(self):
        """Kisziszámolja a globális toxicitási score-t az elmúlt mérések (1s markout) alapján."""
        if not self.recent_markouts:
            self.toxicity_score = 0.0
            return
            
        negative_count = sum(1 for m in self.recent_markouts if m < 0)
        total_count = len(self.recent_markouts)
        
        # Ha a fill-ek több mint fele negatív markoutba megy, mérgezett a book
        ratio = negative_count / total_count
        
        # Súlyozás: a nagyon súlyos negatív értékek dobják meg a pontszámot (TODO: vola alapú súlyozás)
        self.toxicity_score = min(1.0, ratio * 1.5)

    def _log_record(self, record: TradeRecord):
        """Elmenti a kész trade mérést a CSV-be."""
        # Itt a perf_counter() értékeket használjuk a pontos milliszekundumos kiíratáshoz
        lat_sig = int((record.t_quote_sent - record.t_signal) * 1000)
        lat_fill = int((record.t_fill_perf - record.t_quote_sent) * 1000)
        
        with open(self.log_file, "a", newline="") as f:
            writer = csv.writer(f)
            writer.writerow([
                record.wall_clock_time, record.token_id, record.side, record.fill_price, record.fill_size,
                lat_sig, lat_fill,
                record.markout_250ms, record.markout_1s, record.markout_5s
            ])

    def is_toxic(self) -> bool:
        """Kiváltási mérőszám: meghaladta-e a kritikus küszöböt?"""
        return self.toxicity_score >= 0.7
