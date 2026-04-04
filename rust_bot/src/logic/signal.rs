use crate::config::{BollingerConfig, EmaTrendConfig, StrategyConfig};
use std::collections::VecDeque;
use std::time::Instant;

pub struct SignalResult {
    pub side: String,
    pub target_mid: f64,
    /// Karakterisztikus ár-ingadozás (USD / coin egységben), TP/SL távolsághoz; a Z-score ablak árszórása.
    pub volatility: f64,
}

pub trait Indicator {
    fn update(&mut self, price: f64);
    fn evaluate(&self) -> Option<String>;
}

// ==========================================
// 1. Z-SCORE INDIKÁTOR
// ==========================================
struct ZScoreIndicator {
    config: crate::config::ZScoreConfig,
    history: VecDeque<f64>,
}

impl ZScoreIndicator {
    fn new(config: crate::config::ZScoreConfig) -> Self {
        Self { config, history: VecDeque::new() }
    }

    /// Ugyanabból az ablakból számolt ár-szórás (TP/SL skálázás), mint a Z-score.
    fn rolling_price_std(&self) -> Option<f64> {
        if self.history.len() < self.config.window {
            return None;
        }
        let n = self.history.len() as f64;
        let mean: f64 = self.history.iter().sum::<f64>() / n;
        let variance: f64 = self.history.iter().map(|&x| (x - mean).powi(2)).sum::<f64>() / n;
        let std_dev = variance.sqrt();
        if std_dev == 0.0 || !std_dev.is_finite() {
            None
        } else {
            Some(std_dev)
        }
    }
}

impl Indicator for ZScoreIndicator {
    fn update(&mut self, price: f64) {
        self.history.push_back(price);
        if self.history.len() > self.config.window {
            self.history.pop_front();
        }
    }

    fn evaluate(&self) -> Option<String> {
        if !self.config.enabled || self.history.len() < self.config.window {
            return None;
        }
        let mean: f64 = self.history.iter().sum::<f64>() / self.history.len() as f64;
        let variance: f64 = self.history.iter().map(|&x| (x - mean).powi(2)).sum::<f64>() / self.history.len() as f64;
        let std_dev = variance.sqrt();
        if std_dev == 0.0 {
            return None;
        }
        let z_score = (*self.history.back().unwrap() - mean) / std_dev;

        if z_score <= -self.config.threshold {
            Some("Buy".to_string())
        } else if z_score >= self.config.threshold {
            Some("Sell".to_string())
        } else {
            None
        }
    }
}

// ==========================================
// 2. RSI INDIKÁTOR
// ==========================================
struct RsiIndicator {
    config: crate::config::RsiConfig,
    prev_price: Option<f64>,
    avg_gain: f64,
    avg_loss: f64,
    tick_count: usize,
    last_update: Instant,
}

impl RsiIndicator {
    fn new(config: crate::config::RsiConfig) -> Self {
        Self {
            config,
            prev_price: None,
            avg_gain: 0.0,
            avg_loss: 0.0,
            tick_count: 0,
            last_update: Instant::now() - std::time::Duration::from_secs(100),
        }
    }
}

impl Indicator for RsiIndicator {
    fn update(&mut self, price: f64) {
        if self.last_update.elapsed().as_secs() < 5 {
            return;
        }
        self.last_update = Instant::now();

        if let Some(prev) = self.prev_price {
            let change = price - prev;
            let gain = if change > 0.0 { change } else { 0.0 };
            let loss = if change < 0.0 { change.abs() } else { 0.0 };

            if self.tick_count < self.config.window {
                self.avg_gain += gain;
                self.avg_loss += loss;
                self.tick_count += 1;
                if self.tick_count == self.config.window {
                    self.avg_gain /= self.config.window as f64;
                    self.avg_loss /= self.config.window as f64;
                }
            } else {
                self.avg_gain = ((self.avg_gain * (self.config.window as f64 - 1.0)) + gain) / self.config.window as f64;
                self.avg_loss = ((self.avg_loss * (self.config.window as f64 - 1.0)) + loss) / self.config.window as f64;
            }
        }
        self.prev_price = Some(price);
    }

    fn evaluate(&self) -> Option<String> {
        if !self.config.enabled || self.tick_count < self.config.window {
            return None;
        }
        if self.avg_loss == 0.0 {
            return if self.avg_gain > 0.0 { Some("Sell".to_string()) } else { None };
        }
        let rs = self.avg_gain / self.avg_loss;
        let rsi = 100.0 - (100.0 / (1.0 + rs));

        if rsi <= self.config.buy_below {
            Some("Buy".to_string())
        } else if rsi >= self.config.sell_above {
            Some("Sell".to_string())
        } else {
            None
        }
    }
}

// ==========================================
// 3. BOLLINGER (mean reversion: szélső sáv)
// ==========================================
struct BollingerIndicator {
    config: BollingerConfig,
    history: VecDeque<f64>,
}

impl BollingerIndicator {
    fn new(config: BollingerConfig) -> Self {
        Self { config, history: VecDeque::new() }
    }
}

impl Indicator for BollingerIndicator {
    fn update(&mut self, price: f64) {
        self.history.push_back(price);
        if self.history.len() > self.config.window {
            self.history.pop_front();
        }
    }

    fn evaluate(&self) -> Option<String> {
        if !self.config.enabled || self.history.len() < self.config.window {
            return None;
        }
        let n = self.history.len() as f64;
        let mean: f64 = self.history.iter().sum::<f64>() / n;
        let variance: f64 = self.history.iter().map(|&x| (x - mean).powi(2)).sum::<f64>() / n;
        let std_dev = variance.sqrt();
        if std_dev == 0.0 || !std_dev.is_finite() {
            return None;
        }
        let upper = mean + self.config.std_dev * std_dev;
        let lower = mean - self.config.std_dev * std_dev;
        let px = *self.history.back()?;

        if px <= lower {
            Some("Buy".to_string())
        } else if px >= upper {
            Some("Sell".to_string())
        } else {
            None
        }
    }
}

// ==========================================
// 4. EMA TREND FILTER
// ==========================================
struct EmaFilter {
    config: EmaTrendConfig,
    value: Option<f64>,
    k: f64,
}

impl EmaFilter {
    fn new(config: EmaTrendConfig) -> Self {
        let k = if config.window > 0 {
            2.0 / (config.window as f64 + 1.0)
        } else {
            0.0
        };
        Self { config, value: None, k }
    }

    fn update(&mut self, price: f64) {
        match self.value {
            Some(prev) => self.value = Some(prev + self.k * (price - prev)),
            None => self.value = Some(price),
        }
    }

    /// Price vs EMA: Buy allowed if price < EMA (dip into uptrend);
    /// Sell allowed if price > EMA (pop into downtrend).
    fn allows_with_price(&self, side: &str, price: f64) -> bool {
        if !self.config.enabled {
            return true;
        }
        let Some(ema) = self.value else { return true };
        match side {
            "Buy" => price < ema,
            "Sell" => price > ema,
            _ => true,
        }
    }
}

/// Minden beküldött jel opcionális; üres lista = nincs engedélyezett indikátor.
/// Több jelnél mindegyiknek `Some` és azonos iránynak kell lennie.
fn combine_enabled_signals(parts: &[Option<String>]) -> Option<String> {
    if parts.is_empty() {
        return None;
    }
    let first = parts[0].as_ref()?;
    for p in &parts[1..] {
        match p {
            Some(s) if s == first => {}
            _ => return None,
        }
    }
    Some(first.clone())
}

// ==========================================
// MOTOR
// ==========================================
pub struct SignalEngine {
    config: StrategyConfig,
    z_score: ZScoreIndicator,
    rsi: RsiIndicator,
    bollinger: BollingerIndicator,
    ema: EmaFilter,
}

impl SignalEngine {
    pub fn new(config: StrategyConfig) -> Self {
        Self {
            z_score: ZScoreIndicator::new(config.signals.z_score.clone()),
            rsi: RsiIndicator::new(config.signals.rsi.clone()),
            bollinger: BollingerIndicator::new(config.signals.bollinger.clone()),
            ema: EmaFilter::new(config.signals.filters.ema_trend.clone()),
            config,
        }
    }

    /// `imbalance`: orderbook bid-side ratio from feed (0.0 = all asks, 1.0 = all bids, 0.5 = balanced).
    pub async fn tick(&mut self, mid: f64, imbalance: f64) -> Option<SignalResult> {
        if mid <= 0.0 {
            return None;
        }

        self.z_score.update(mid);
        self.rsi.update(mid);
        self.bollinger.update(mid);
        self.ema.update(mid);

        let z_sig = self.z_score.evaluate();
        let rsi_sig = self.rsi.evaluate();
        let boll_sig = self.bollinger.evaluate();

        let z_on = self.config.signals.z_score.enabled;
        let rsi_on = self.config.signals.rsi.enabled;
        let boll_on = self.config.signals.bollinger.enabled;

        let mut parts: Vec<Option<String>> = Vec::new();
        if z_on {
            parts.push(z_sig);
        }
        if rsi_on {
            parts.push(rsi_sig);
        }
        if boll_on {
            parts.push(boll_sig);
        }

        let final_sig = combine_enabled_signals(&parts)?;

        let vol_px = self
            .z_score
            .rolling_price_std()
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(mid * 0.005);

        // --- FILTER 1: EMA trend ---
        if !self.ema.allows_with_price(&final_sig, mid) {
            tracing::debug!(
                "Signal {} blocked by EMA trend filter (mid={:.2}, ema={:.2})",
                final_sig,
                mid,
                self.ema.value.unwrap_or(0.0)
            );
            return None;
        }

        // --- FILTER 2: Volatility gate ---
        let vg = &self.config.signals.filters.vol_gate;
        if vg.enabled && mid > 0.0 {
            let vol_pct = (vol_px / mid) * 100.0;
            if vol_pct > vg.max_vol_pct {
                tracing::debug!(
                    "Signal {} blocked by vol gate ({:.3}% > {:.1}% max)",
                    final_sig,
                    vol_pct,
                    vg.max_vol_pct
                );
                return None;
            }
        }

        // --- FILTER 3: Orderbook imbalance ---
        let imb = &self.config.signals.filters.imbalance;
        if imb.enabled {
            let blocked = match final_sig.as_str() {
                "Sell" => imbalance > imb.block_threshold,
                "Buy" => imbalance < (1.0 - imb.block_threshold),
                _ => false,
            };
            if blocked {
                tracing::debug!(
                    "Signal {} blocked by imbalance filter (imb={:.3}, threshold={:.2})",
                    final_sig,
                    imbalance,
                    imb.block_threshold
                );
                return None;
            }
        }

        Some(SignalResult {
            side: final_sig,
            target_mid: mid,
            volatility: vol_px,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combine_requires_all_same() {
        assert_eq!(
            combine_enabled_signals(&[Some("Buy".to_string())]),
            Some("Buy".to_string())
        );
        assert_eq!(
            combine_enabled_signals(&[Some("Buy".to_string()), Some("Buy".to_string())]),
            Some("Buy".to_string())
        );
        assert_eq!(
            combine_enabled_signals(&[Some("Buy".to_string()), Some("Sell".to_string())]),
            None
        );
        assert_eq!(combine_enabled_signals(&[None]), None);
        assert_eq!(
            combine_enabled_signals(&[Some("Buy".to_string()), None]),
            None
        );
    }

    #[test]
    fn ema_filter_blocks_counter_trend() {
        let cfg = EmaTrendConfig { enabled: true, window: 5 };
        let mut ema = EmaFilter::new(cfg);
        // Simulate rising price: EMA trails below → should block Buy, allow Sell
        for p in [100.0, 101.0, 102.0, 103.0, 104.0, 105.0] {
            ema.update(p);
        }
        assert!(ema.value.unwrap() < 105.0);
        assert!(ema.allows_with_price("Sell", 105.0));
        assert!(!ema.allows_with_price("Buy", 105.0));

        // Simulate falling price
        for p in [90.0, 89.0, 88.0, 87.0, 86.0, 85.0] {
            ema.update(p);
        }
        assert!(ema.value.unwrap() > 85.0);
        assert!(ema.allows_with_price("Buy", 85.0));
        assert!(!ema.allows_with_price("Sell", 85.0));
    }

    #[test]
    fn ema_filter_disabled_allows_all() {
        let cfg = EmaTrendConfig { enabled: false, window: 5 };
        let mut ema = EmaFilter::new(cfg);
        ema.update(100.0);
        assert!(ema.allows_with_price("Buy", 105.0));
        assert!(ema.allows_with_price("Sell", 95.0));
    }
}
