use crate::config::StrategyConfig;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub struct SignalResult {
    pub side: String,
    pub target_mid: f64,
    /// Karakterisztikus ár-ingadozás (USD), TP/SL skálázáshoz.
    pub volatility: f64,
}

// ==========================================
// 1. LIQUIDITY SWEEP DETEKTOR
// ==========================================
struct SweepEngine {
    window: usize,
    threshold_pct: f64,
    prices: VecDeque<f64>,
    swept_low: bool,
    swept_high: bool,
    sweep_low_level: f64,
    sweep_high_level: f64,
    // O(1) rolling min/max tracking
    running_high: f64,
    running_low: f64,
    // Visszapattanás-megerősítés: megakadályozza, hogy folytatódó esésbe longgoljunk be.
    confirmation_pct: f64,
    confirm_ticks_required: usize,
    pending_buy_level: Option<f64>,  // sweep_low_level az észlelés pillanatában
    pending_sell_level: Option<f64>, // sweep_high_level az észlelés pillanatában
    buy_confirm_ticks: usize,
    sell_confirm_ticks: usize,
    // Söprés-trigger ár: az az ár, amelyen a söprés éppen tüzelt (mid a detektálás pillanatában).
    // Érvénytelenítés: ha az ár az eredeti trigger szint alá/fölé megy (nem a window low/high-hoz képest),
    // az azonnali false invalidálást kerüljük el — a sell sweep különben minden esetben azonnal törlődne.
    pending_buy_trigger: f64,
    pending_sell_trigger: f64,
}

impl SweepEngine {
    fn new(window: usize, threshold_pct: f64, confirmation_pct: f64, confirm_ticks_required: usize) -> Self {
        Self {
            window,
            threshold_pct,
            prices: VecDeque::new(),
            swept_low: false,
            swept_high: false,
            sweep_low_level: f64::NAN,
            sweep_high_level: f64::NAN,
            running_high: f64::NEG_INFINITY,
            running_low: f64::INFINITY,
            confirmation_pct,
            confirm_ticks_required,
            pending_buy_level: None,
            pending_sell_level: None,
            buy_confirm_ticks: 0,
            sell_confirm_ticks: 0,
            pending_buy_trigger: 0.0,
            pending_sell_trigger: f64::INFINITY,
        }
    }

    fn tick(&mut self, mid: f64) -> (bool, bool) {
        // Teljes ablak kell a futó min/max megbízhatóságához — a fél-ablakos korai start
        // részleges historával hamis söprés-szinteket produkálna.
        let has_history = self.prices.len() >= self.window;

        let (recent_high, recent_low) = if has_history {
            (self.running_high, self.running_low)
        } else {
            if self.prices.len() >= self.window { self.prices.pop_front(); }
            self.prices.push_back(mid);
            self.running_high = self.running_high.max(mid);
            self.running_low = self.running_low.min(mid);
            return (false, false);
        };

        // A megerősítés ellenőrzése az ELŐZŐ tick-beli függő állapotra fut —
        // nem a most észlelt söprésre. Ezért mentjük el a pending-et a söprés-detektálás ELŐTT.
        let prev_pending_buy  = self.pending_buy_level;
        let prev_pending_sell = self.pending_sell_level;

        let thresh_up = recent_high * (1.0 + self.threshold_pct / 100.0);
        let thresh_down = recent_low * (1.0 - self.threshold_pct / 100.0);

        // Söprés észlelése → függő szignál állapotba lép (csak a KÖVETKEZŐ tick értékeli ki)
        if mid >= thresh_up && self.pending_sell_level.is_none() {
            self.swept_high = true;
            self.sweep_high_level = recent_high;
            self.pending_sell_level = Some(recent_high);
            self.pending_sell_trigger = mid; // a tényleges trigger ár (nem az ablak csúcs)
            self.sell_confirm_ticks = 0;
        }
        if mid <= thresh_down && self.pending_buy_level.is_none() {
            self.swept_low = true;
            self.sweep_low_level = recent_low;
            self.pending_buy_level = Some(recent_low);
            self.pending_buy_trigger = mid; // a tényleges trigger ár (nem az ablak mélypont)
            self.buy_confirm_ticks = 0;
        }

        if self.prices.len() >= self.window {
            if let Some(evicted) = self.prices.pop_front() {
                if (evicted - self.running_high).abs() < 1e-12
                    || (evicted - self.running_low).abs() < 1e-12
                {
                    self.running_high = self.prices.iter().cloned().fold(mid, f64::max);
                    self.running_low = self.prices.iter().cloned().fold(mid, f64::min);
                }
            }
        }
        self.prices.push_back(mid);
        self.running_high = self.running_high.max(mid);
        self.running_low = self.running_low.min(mid);

        let mut buy_sig = false;
        let mut sell_sig = false;

        // Függő BUY megerősítés (az előző tick óta fennálló pending-re)
        if let Some(swept_level) = prev_pending_buy {
            if mid < self.pending_buy_trigger {
                // Ár az eredeti sweep trigger szint alá esett → folytatódó esés, töröljük.
                // (Nem az ablak mélypont a határ — az azonnal invalidálná a normális visszapattanást.)
                self.pending_buy_level = None;
                self.swept_low = false;
                self.buy_confirm_ticks = 0;
            } else {
                let confirm_thresh = swept_level * (1.0 + self.confirmation_pct / 100.0);
                if mid >= confirm_thresh {
                    self.buy_confirm_ticks += 1;
                }
                if self.buy_confirm_ticks >= self.confirm_ticks_required {
                    buy_sig = true;
                    self.pending_buy_level = None;
                    self.swept_low = false;
                    self.buy_confirm_ticks = 0;
                }
            }
        }

        // Függő SELL megerősítés (az előző tick óta fennálló pending-re)
        if let Some(swept_level) = prev_pending_sell {
            if mid > self.pending_sell_trigger {
                // Ár az eredeti sweep trigger szint fölé emelkedett → folytatódó emelkedés, töröljük.
                // (Nem az ablak csúcs a határ — az azonnal invalidálná, mert a sweep ár már fölötte volt.)
                self.pending_sell_level = None;
                self.swept_high = false;
                self.sell_confirm_ticks = 0;
            } else {
                let confirm_thresh = swept_level * (1.0 - self.confirmation_pct / 100.0);
                if mid <= confirm_thresh {
                    self.sell_confirm_ticks += 1;
                }
                if self.sell_confirm_ticks >= self.confirm_ticks_required {
                    sell_sig = true;
                    self.pending_sell_level = None;
                    self.swept_high = false;
                    self.sell_confirm_ticks = 0;
                }
            }
        }

        (buy_sig, sell_sig)
    }
}

// ==========================================
// 2. ORDER FLOW (kumulatív imbalance score)
// ==========================================
struct FlowEngine {
    window: usize,
    threshold: f64,
    history: VecDeque<f64>,
    cumulative: f64,
}

impl FlowEngine {
    fn new(window: usize, threshold: f64) -> Self {
        Self { window, threshold, history: VecDeque::new(), cumulative: 0.0 }
    }

    fn tick(&mut self, imbalance: f64) -> (bool, bool) {
        // Nem-lineáris (köbös) skálázás: 0.95 imbalance >> 0.6 imbalance.
        // d ∈ [-0.5, 0.5] → scaled ∈ [-1, 1] → köbös → delta ∈ [-0.25, 0.25].
        let d = imbalance - 0.5;
        let s = d * 2.0;
        let delta = s * s * s * 0.25;

        if self.history.len() >= self.window {
            if let Some(old) = self.history.pop_front() {
                self.cumulative -= old;
            }
        }
        self.history.push_back(delta);
        self.cumulative += delta;
        (self.cumulative > self.threshold, self.cumulative < -self.threshold)
    }
}

// ==========================================
// 3. VWAP ENGINE
//    Elsődleges: Volume-weighted VWAP (a feed trades streamjéből).
//    Fallback:   TWAP (időn alapuló átlag), ha még nincs elég trades adat.
// ==========================================
struct VwapEngine {
    session_duration: Duration,
    deviation_pct: f64,
    // TWAP fallback belső állapot
    session_start: Option<Instant>,
    sum_price: f64,
    count: u64,
    twap: f64,
}

impl VwapEngine {
    fn new(session_hours: u32, deviation_pct: f64) -> Self {
        Self {
            session_duration: Duration::from_secs(session_hours as u64 * 3600),
            deviation_pct,
            session_start: None,
            sum_price: 0.0,
            count: 0,
            twap: 0.0,
        }
    }

    /// `volume_vwap`: a feed által számított Volume VWAP.
    /// - Ha > 0: ezt használja (igazi forgalom-súlyozott VWAP).
    /// - Ha == 0: TWAP fallback (bot induláskor, amíg a trades stream feltölt).
    fn tick(&mut self, mid: f64, volume_vwap: f64) -> (bool, bool) {
        // TWAP mindig frissül (fallback + self-check)
        let now = Instant::now();
        if self.session_start.map_or(true, |s| now.duration_since(s) >= self.session_duration) {
            self.session_start = Some(now);
            self.sum_price = 0.0;
            self.count = 0;
        }
        self.sum_price += mid;
        self.count += 1;
        self.twap = self.sum_price / self.count as f64;

        // Effektív VWAP: volume_vwap ha elérhető, különben TWAP
        let effective_vwap = if volume_vwap > 0.0 { volume_vwap } else { self.twap };

        // Warmup: ha TWAP-ra támaszkodunk, várjunk legalább 30 mintát
        if effective_vwap <= 0.0 || (volume_vwap <= 0.0 && self.count < 30) {
            return (false, false);
        }

        let lower = effective_vwap * (1.0 - self.deviation_pct / 100.0);
        let upper = effective_vwap * (1.0 + self.deviation_pct / 100.0);
        (mid < lower, mid > upper)
    }
}

// ==========================================
// 4. REGIME FILTER (EMA-alapú trend-szűrő)
// ==========================================
// Időalapú mintavételezés: minden `sample_secs` másodpercben frissíti az EMA-kat.
// Alapértelmezett konfig: 30s minta → EMA-50 ≈ 25 perc, EMA-200 ≈ 100 perc (1h40m).
// Ez az 1-2 órás piaci struktúrát adja vissza, amit az orderbook tick-ek nem látnak.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Regime {
    Uptrend,
    Downtrend,
    Sideways,
}

impl Regime {
    fn label(self) -> &'static str {
        match self {
            Regime::Uptrend   => "UPTREND",
            Regime::Downtrend => "DOWNTREND",
            Regime::Sideways  => "SIDEWAYS",
        }
    }
}

struct RegimeEngine {
    fast_alpha: f64,
    slow_alpha: f64,
    fast_ema: f64,
    slow_ema: f64,
    band_pct: f64,
    sample_secs: u64,
    last_sample: Instant,
    acc_sum: f64,
    acc_count: u64,
    samples_total: u64,
    /// Ennyi mintára van szükség, mielőtt trend-jelzést adunk (slow EMA konvergencia).
    min_samples: u64,
    prev_regime: Regime,
    log_counter: u32,
}

impl RegimeEngine {
    fn new(fast_period: usize, slow_period: usize, sample_secs: u64, band_pct: f64) -> Self {
        Self {
            fast_alpha: 2.0 / (fast_period as f64 + 1.0),
            slow_alpha: 2.0 / (slow_period as f64 + 1.0),
            fast_ema: 0.0,
            slow_ema: 0.0,
            band_pct,
            sample_secs,
            last_sample: Instant::now(),
            acc_sum: 0.0,
            acc_count: 0,
            samples_total: 0,
            // Slow EMA konvergencia: ~1× slow_period elegendő (exponenciális lecsengés)
            // Warmup: slow_period / 4 minta elég az EMA-szóródás értelmezéséhez
            // (mindkét EMA ugyanonnan indul → tényleges spread = tényleges trend).
            // 200/4 = 50 minta × 30s = 25 perc. A korábbi slow_period (100 perc) alatt
            // SIDEWAYS volt az egész melegítési időszak → downtrend közben is longholt a bot.
            min_samples: (slow_period / 4).max(2) as u64,
            prev_regime: Regime::Sideways,
            log_counter: 0,
        }
    }

    fn tick(&mut self, mid: f64) -> Regime {
        // Minden tick: akkumulálás
        self.acc_sum += mid;
        self.acc_count += 1;

        // Mintavételezési periódus nem telt le → jelenlegi rezsim visszaadása
        if self.last_sample.elapsed().as_secs() < self.sample_secs {
            return self.prev_regime;
        }

        // Mintavételezési periódus lejárt: EMA frissítés
        let sample_px = if self.acc_count > 0 {
            self.acc_sum / self.acc_count as f64
        } else {
            mid
        };
        self.acc_sum = 0.0;
        self.acc_count = 0;
        self.last_sample = Instant::now();
        self.samples_total += 1;

        if self.fast_ema == 0.0 {
            // Inicializálás: mindkét EMA az első árra áll
            self.fast_ema = sample_px;
            self.slow_ema = sample_px;
            return Regime::Sideways;
        }

        self.fast_ema = sample_px * self.fast_alpha + self.fast_ema * (1.0 - self.fast_alpha);
        self.slow_ema = sample_px * self.slow_alpha + self.slow_ema * (1.0 - self.slow_alpha);

        let regime = if self.samples_total < self.min_samples {
            // Slow EMA még konvergál → semleges
            Regime::Sideways
        } else {
            let spread_pct = (self.fast_ema - self.slow_ema) / self.slow_ema * 100.0;
            if spread_pct > self.band_pct {
                Regime::Uptrend
            } else if spread_pct < -self.band_pct {
                Regime::Downtrend
            } else {
                Regime::Sideways
            }
        };

        // Rezsimváltás vagy 10 percenkénti periodikus log
        self.log_counter += 1;
        let changed = regime != self.prev_regime;
        if changed || self.log_counter >= 20 {
            let spread = (self.fast_ema - self.slow_ema) / self.slow_ema * 100.0;
            tracing::info!(
                "📊 Rezsim: {} | EMA-fast={:.4} EMA-slow={:.4} spread={:+.3}% | minták={}",
                regime.label(), self.fast_ema, self.slow_ema, spread, self.samples_total
            );
            self.log_counter = 0;
        }
        self.prev_regime = regime;
        regime
    }
}

// ==========================================
// MOTOR
// ==========================================
pub struct SignalEngine {
    sweep: SweepEngine,
    flow: FlowEngine,
    vwap: VwapEngine,
    regime: RegimeEngine,
    prev_mid: Option<f64>,
    return_history: VecDeque<f64>,
    sum_x: f64,
    sum_x2: f64,
    tick_count: u64,
    min_vol_pct: Option<f64>,
    max_vol_pct: Option<f64>,
}

impl SignalEngine {
    pub fn new(config: StrategyConfig) -> Self {
        Self {
            sweep: SweepEngine::new(
                config.sweep_window as usize,
                config.sweep_threshold_pct,
                config.sweep_confirmation_pct,
                config.sweep_confirm_ticks as usize,
            ),
            flow: FlowEngine::new(config.flow_window as usize, config.flow_threshold),
            vwap: VwapEngine::new(config.vwap_session_hours, config.vwap_deviation_pct),
            regime: RegimeEngine::new(
                config.regime_fast_period as usize,
                config.regime_slow_period as usize,
                config.regime_sample_secs,
                config.regime_band_pct,
            ),
            prev_mid: None,
            return_history: VecDeque::new(),
            sum_x: 0.0,
            sum_x2: 0.0,
            tick_count: 0,
            min_vol_pct: config.min_vol_pct,
            max_vol_pct: config.max_vol_pct,
        }
    }

    /// CPU-only számítás — nincs I/O, nem blokkol aszinkron executort.
    ///
    /// `volume_vwap`: feed által számított Volume VWAP (0.0 = nincs adat → TWAP fallback).
    pub fn tick(&mut self, mid: f64, imbalance: f64, volume_vwap: f64) -> Option<SignalResult> {
        if mid <= 0.0 { return None; }
        self.tick_count += 1;
        let volatility = self.update_volatility(mid);

        // Volatilitás-rezsim határok: csendes piacban a spread felemésztené az edge-et,
        // viharos piacban a diffúzió az SL-t söpörné el.
        let vol_pct = volatility / mid * 100.0;
        if let Some(min_v) = self.min_vol_pct {
            if vol_pct < min_v && self.tick_count > 200 {
                return None;
            }
        }
        if let Some(max_v) = self.max_vol_pct {
            if vol_pct > max_v {
                return None;
            }
        }

        let (sweep_buy, sweep_sell) = self.sweep.tick(mid);
        let (flow_bull, flow_bear) = self.flow.tick(imbalance);
        let (below_vwap, above_vwap) = self.vwap.tick(mid, volume_vwap);
        let regime = self.regime.tick(mid);

        // Rezsim-szűrő: mindkét szignáltípusra egységesen.
        // DOWNTREND: ne vegyünk (eső trend ellen long = catching falling knife).
        // UPTREND:   ne adjunk el (emelkedő trend ellen short = szükségtelen kockázat).
        // SIDEWAYS:  mindkét irány szabad (mean-reversion a legjobb ilyen piacon).
        let buy_allowed  = regime != Regime::Downtrend;
        let sell_allowed = regime != Regime::Uptrend;

        // 1. Likviditás-söpör visszafordulás — rezsim-szűréssel.
        //    Mindkét megerősítő szignál (flow + VWAP) szükséges, hogy a sweep ne tüzeljen
        //    gyenge / egyoldalú megerősítéssel; a flow+VWAP ág is ezt a feltételrendszert alkalmazza.
        if sweep_buy && flow_bull && below_vwap && buy_allowed {
            return Some(SignalResult { side: "Buy".to_string(), target_mid: mid, volatility });
        }
        if sweep_sell && flow_bear && above_vwap && sell_allowed {
            return Some(SignalResult { side: "Sell".to_string(), target_mid: mid, volatility });
        }

        // 2. Flow + VWAP mean-reversion — szintén rezsim-szűréssel.
        //
        //    Logika:
        //    - UPTREND:   ne adjunk el (VWAP feletti ár normális; ne shortoljuk a trendet)
        //                 Long belépés (dip VWAP alá uptrendben) engedett.
        //    - DOWNTREND: ne vegyünk (VWAP alatti ár normális; ne longjuk a trendet)
        //                 Short belépés (rally VWAP fölé downtrendben) engedett.
        //    - SIDEWAYS:  mindkét irány szabad (mean reversion a legjobb ilyen piacon).
        if self.tick_count > 100 {
            if flow_bull && below_vwap && buy_allowed {
                return Some(SignalResult { side: "Buy".to_string(), target_mid: mid, volatility });
            }
            if flow_bear && above_vwap && sell_allowed {
                return Some(SignalResult { side: "Sell".to_string(), target_mid: mid, volatility });
            }
        }

        None
    }

    fn update_volatility(&mut self, mid: f64) -> f64 {
        const HIST: usize = 200;
        if self.tick_count % 1000 == 0 && !self.return_history.is_empty() {
            self.sum_x = self.return_history.iter().sum();
            self.sum_x2 = self.return_history.iter().map(|&x| x * x).sum();
        }
        let Some(prev) = self.prev_mid else {
            self.prev_mid = Some(mid);
            return mid * 0.005;
        };
        self.prev_mid = Some(mid);
        if prev <= 0.0 { return mid * 0.005; }
        let log_ret = (mid / prev).ln();
        if self.return_history.len() >= HIST {
            if let Some(old) = self.return_history.pop_front() {
                self.sum_x -= old;
                self.sum_x2 -= old * old;
            }
        }
        self.return_history.push_back(log_ret);
        self.sum_x += log_ret;
        self.sum_x2 += log_ret * log_ret;
        let n = self.return_history.len() as f64;
        if n < 10.0 { return mid * 0.005; }
        let mean = self.sum_x / n;
        let variance = ((self.sum_x2 / n) - (mean * mean)).max(0.0);
        (variance.sqrt() * mid).abs().max(mid * 0.001)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_buy_signal_on_low_sweep_and_reversal() {
        // confirmation_pct=0.0, confirm_ticks=1 → az első visszakerülés a söprés szintje fölé tüzel
        // Teljes ablak (window=10) kell a has_history=true-hoz.
        let mut engine = SweepEngine::new(10, 0.10, 0.0, 1);
        for p in [100.0_f64, 100.1, 99.9, 100.05, 100.15, 100.0, 100.1, 99.95, 100.08, 100.12] {
            let _ = engine.tick(p);
        }
        let _ = engine.tick(99.75); // sweep_low = true, pending_buy beállítva (swept_level≈99.9)
        let (buy, sell) = engine.tick(100.0); // 100.0 >= 99.9 → confirm_ticks=1 >= 1 → tüzel
        assert!(buy, "Buy szignálnak kellene tüzelni sweep+reversal után");
        assert!(!sell);
    }

    #[test]
    fn sweep_no_signal_without_sweep() {
        let mut engine = SweepEngine::new(10, 0.10, 0.0, 1);
        for p in [100.0_f64, 100.05, 99.95, 100.02, 100.03, 100.01] {
            let (buy, sell) = engine.tick(p);
            assert!(!buy && !sell);
        }
    }

    #[test]
    fn sweep_buy_invalidated_on_continuation() {
        // Hamis visszapattanás: söprés után az ár tovább esik → nincs szignál
        let mut engine = SweepEngine::new(10, 0.10, 0.05, 1);
        // Teljes ablak feltöltése (window=10)
        for p in [100.0_f64, 100.1, 99.9, 100.05, 100.15, 100.0, 100.1, 99.95, 100.08, 100.12] {
            let _ = engine.tick(p);
        }
        // Söprés: ár a recent_low (99.9) * 0.999 = ~99.8001 alá megy
        let _ = engine.tick(99.75); // pending_buy beállítva (swept_level ≈ 99.9)
        // Ár kissé visszajön, de a confirmation_pct (0.05%) thresholdet nem éri el
        let _ = engine.tick(99.85); // 99.85 < 99.9 * 1.0005 = 99.9499 → confirm_ticks nem nő
        // Ár újabb mélypontra esik → invalidálás
        let (buy, sell) = engine.tick(99.70); // mid < swept_level → pending törlődik
        assert!(!buy, "Folytatódó esésbe NEM szabad buy szignált adni");
        assert!(!sell);
    }

    #[test]
    fn sweep_buy_fires_after_confirmation() {
        // Valós visszapattanás: sweep + elegendő recovery → szignál
        let mut engine = SweepEngine::new(10, 0.10, 0.05, 2);
        // Teljes ablak feltöltése (window=10)
        for p in [100.0_f64, 100.1, 99.9, 100.05, 100.15, 100.0, 100.1, 99.95, 100.08, 100.12] {
            let _ = engine.tick(p);
        }
        // Söprés (swept_level ≈ 99.9)
        let _ = engine.tick(99.75);
        // 1. confirming tick: 99.9 * 1.0005 = 99.9499 < 100.05 → buy_confirm_ticks=1
        let (b1, _) = engine.tick(100.05);
        assert!(!b1, "1 confirming tick még nem elég (confirm_ticks=2)");
        // 2. confirming tick → buy_confirm_ticks=2 >= 2 → tüzel
        let (b2, s2) = engine.tick(100.10);
        assert!(b2, "2 confirming tick után buy szignálnak kell tüzelni");
        assert!(!s2);
    }

    #[test]
    fn flow_bullish_on_sustained_bid_pressure() {
        // Köbös skálán: 0.9 imbalance → d=0.4, s=0.8, delta=0.8³×0.25≈0.128/tick
        // threshold=1.0 → 8 tick után triggerel; 11 tick >> elegendő
        let mut engine = FlowEngine::new(10, 1.0);
        for _ in 0..11 { let _ = engine.tick(0.9); }
        let (bull, bear) = engine.tick(0.9);
        assert!(bull, "köbös 0.9 imbalance 11 ticken: bull kell");
        assert!(!bear);
    }

    #[test]
    fn flow_neutral_imbalance_never_triggers() {
        // 0.6 imbalance → d=0.1, s=0.2, delta=0.008/tick; 50 tick cumul=0.4 < 1.0 threshold
        let mut engine = FlowEngine::new(50, 1.0);
        for _ in 0..50 { let _ = engine.tick(0.6); }
        let (bull, _bear) = engine.tick(0.6);
        assert!(!bull, "mérsékelt 0.6 imbalance nem triggerelheti a flow engine-t");
    }

    #[test]
    fn vwap_below_signal_twap_fallback() {
        // volume_vwap=0.0 → TWAP fallback; TWAP≈100.0 után 99.4 < lower
        let mut engine = VwapEngine::new(8, 0.5);
        for _ in 0..100 { engine.tick(100.0, 0.0); }
        let (below, above) = engine.tick(99.4, 0.0);
        assert!(below, "TWAP fallback: 99.4 alatta kell legyen");
        assert!(!above);
    }

    #[test]
    fn vwap_below_signal_volume_vwap() {
        // volume_vwap=100.0 → ezt használja, nem a TWAP-ot
        let mut engine = VwapEngine::new(8, 0.5);
        // Csak néhány tick (kevesebb mint 30) — TWAP fallback nem lenne elég
        for _ in 0..5 { engine.tick(50.0, 0.0); } // TWAP=50, de ezt nem kellene használni
        let (below, above) = engine.tick(99.4, 100.0); // volume_vwap=100.0
        assert!(below, "volume VWAP=100.0 esetén 99.4 alatta kell legyen");
        assert!(!above);
    }

    #[test]
    fn regime_sideways_during_warmup() {
        // Slow EMA min_samples=5 szükséges; kevesebb minta → SIDEWAYS
        let mut engine = RegimeEngine::new(3, 5, 0, 0.05); // sample_secs=0 → minden ticknél frissít
        for _ in 0..3 {
            assert_eq!(engine.tick(100.0), Regime::Sideways, "warmup alatt SIDEWAYS kell");
        }
    }

    #[test]
    fn regime_detects_uptrend() {
        // sample_secs=0: minden tick minta; band=0.0: minimális spread elég
        let mut engine = RegimeEngine::new(3, 5, 0, 0.0);
        // Növekvő árak → fast EMA > slow EMA
        let prices = [100.0, 101.0, 102.0, 103.0, 104.0, 105.0, 106.0, 107.0, 108.0, 109.0, 110.0];
        let mut last = Regime::Sideways;
        for &p in &prices { last = engine.tick(p); }
        assert_eq!(last, Regime::Uptrend, "tartós emelkedés után UPTREND kell");
    }

    #[test]
    fn regime_detects_downtrend() {
        let mut engine = RegimeEngine::new(3, 5, 0, 0.0);
        let prices = [110.0, 109.0, 108.0, 107.0, 106.0, 105.0, 104.0, 103.0, 102.0, 101.0, 100.0];
        let mut last = Regime::Sideways;
        for &p in &prices { last = engine.tick(p); }
        assert_eq!(last, Regime::Downtrend, "tartós esés után DOWNTREND kell");
    }
}
