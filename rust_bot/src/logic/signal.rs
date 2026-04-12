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
}

impl SweepEngine {
    fn new(window: usize, threshold_pct: f64) -> Self {
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
        }
    }

    fn tick(&mut self, mid: f64) -> (bool, bool) {
        let has_history = self.prices.len() >= self.window / 2;

        let (recent_high, recent_low) = if has_history {
            (self.running_high, self.running_low)
        } else {
            if self.prices.len() >= self.window { self.prices.pop_front(); }
            self.prices.push_back(mid);
            self.running_high = self.running_high.max(mid);
            self.running_low = self.running_low.min(mid);
            return (false, false);
        };

        let thresh_up = recent_high * (1.0 + self.threshold_pct / 100.0);
        let thresh_down = recent_low * (1.0 - self.threshold_pct / 100.0);

        if mid >= thresh_up {
            self.swept_high = true;
            self.sweep_high_level = recent_high;
        }
        if mid <= thresh_down {
            self.swept_low = true;
            self.sweep_low_level = recent_low;
        }

        // Rolling window update: ha a legrégebbi árat dobjuk ki és az volt a min/max,
        // újra kell számolni — ez O(n) de ritkán fordul elő (csak ablak teli esetén).
        if self.prices.len() >= self.window {
            if let Some(evicted) = self.prices.pop_front() {
                if (evicted - self.running_high).abs() < 1e-12
                    || (evicted - self.running_low).abs() < 1e-12
                {
                    // Kiesett a min vagy max — teljes újraszámítás szükséges
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
        if self.swept_low && mid > self.sweep_low_level {
            buy_sig = true;
            self.swept_low = false;
        }
        if self.swept_high && mid < self.sweep_high_level {
            sell_sig = true;
            self.swept_high = false;
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
        // Lineárisnál 0.6 vs 0.95 = 4.5× különbség; köbösnel ~60×.
        let d = imbalance - 0.5;
        let s = d * 2.0;                        // normálva [-1, 1]
        let delta = s * s * s * 0.25;           // d^3 * 0.25, max ±0.25

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
// 3. ANCHORED VWAP (TWAP, nincs volumen adat)
// ==========================================
struct VwapEngine {
    session_duration: Duration,
    deviation_pct: f64,
    session_start: Option<Instant>,
    sum_price: f64,
    count: u64,
    vwap: f64,
}

impl VwapEngine {
    fn new(session_hours: u32, deviation_pct: f64) -> Self {
        Self {
            session_duration: Duration::from_secs(session_hours as u64 * 3600),
            deviation_pct,
            session_start: None,
            sum_price: 0.0,
            count: 0,
            vwap: 0.0,
        }
    }

    fn tick(&mut self, mid: f64) -> (bool, bool) {
        let now = Instant::now();
        if self.session_start.map_or(true, |s| now.duration_since(s) >= self.session_duration) {
            self.session_start = Some(now);
            self.sum_price = 0.0;
            self.count = 0;
        }
        self.sum_price += mid;
        self.count += 1;
        self.vwap = self.sum_price / self.count as f64;

        if self.vwap <= 0.0 || self.count < 30 {
            return (false, false);
        }
        let lower = self.vwap * (1.0 - self.deviation_pct / 100.0);
        let upper = self.vwap * (1.0 + self.deviation_pct / 100.0);
        (mid < lower, mid > upper)
    }
}

// ==========================================
// MOTOR
// ==========================================
pub struct SignalEngine {
    sweep: SweepEngine,
    flow: FlowEngine,
    vwap: VwapEngine,
    prev_mid: Option<f64>,
    return_history: VecDeque<f64>,
    sum_x: f64,
    sum_x2: f64,
    tick_count: u64,
}

impl SignalEngine {
    pub fn new(config: StrategyConfig) -> Self {
        Self {
            sweep: SweepEngine::new(config.sweep_window as usize, config.sweep_threshold_pct),
            flow: FlowEngine::new(config.flow_window as usize, config.flow_threshold),
            vwap: VwapEngine::new(config.vwap_session_hours, config.vwap_deviation_pct),
            prev_mid: None,
            return_history: VecDeque::new(),
            sum_x: 0.0,
            sum_x2: 0.0,
            tick_count: 0,
        }
    }

    /// CPU-only számítás — nincs I/O, nem blokkol aszinkron executort.
    pub fn tick(&mut self, mid: f64, imbalance: f64) -> Option<SignalResult> {
        if mid <= 0.0 { return None; }
        self.tick_count += 1;
        let volatility = self.update_volatility(mid);

        let (sweep_buy, sweep_sell) = self.sweep.tick(mid);
        let (flow_bull, flow_bear) = self.flow.tick(imbalance);
        let (below_vwap, above_vwap) = self.vwap.tick(mid);

        // 1. Liquidity sweep reversal (eredeti logika)
        if sweep_buy && (flow_bull || below_vwap) {
            return Some(SignalResult { side: "Buy".to_string(), target_mid: mid, volatility });
        }
        if sweep_sell && (flow_bear || above_vwap) {
            return Some(SignalResult { side: "Sell".to_string(), target_mid: mid, volatility });
        }

        // 2. Flow + VWAP mean-reversion (sweep nélkül — trendező/csendes piacon is nyit)
        // Elég ha az orderbook irányított ÉS az ár eltér a session VWAP-tól.
        if self.tick_count > 100 {
            if flow_bull && below_vwap {
                return Some(SignalResult { side: "Buy".to_string(), target_mid: mid, volatility });
            }
            if flow_bear && above_vwap {
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
        let mut engine = SweepEngine::new(10, 0.10);
        for p in [100.0_f64, 100.1, 99.9, 100.05, 100.15] {
            let _ = engine.tick(p);
        }
        let _ = engine.tick(99.75); // sweep_low = true
        let (buy, sell) = engine.tick(100.0);
        assert!(buy, "Buy szignálnak kellene tüzelni sweep+reversal után");
        assert!(!sell);
    }

    #[test]
    fn sweep_no_signal_without_sweep() {
        let mut engine = SweepEngine::new(10, 0.10);
        for p in [100.0_f64, 100.05, 99.95, 100.02, 100.03, 100.01] {
            let (buy, sell) = engine.tick(p);
            assert!(!buy && !sell);
        }
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
    fn vwap_below_signal() {
        let mut engine = VwapEngine::new(8, 0.5);
        for _ in 0..100 { engine.tick(100.0); }
        let (below, above) = engine.tick(99.4);
        assert!(below);
        assert!(!above);
    }
}
