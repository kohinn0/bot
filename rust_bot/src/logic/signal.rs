use crate::config::{BollingerConfig, StrategyConfig};
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
}

impl SignalEngine {
    pub fn new(config: StrategyConfig) -> Self {
        Self {
            z_score: ZScoreIndicator::new(config.signals.z_score.clone()),
            rsi: RsiIndicator::new(config.signals.rsi.clone()),
            bollinger: BollingerIndicator::new(config.signals.bollinger.clone()),
            config,
        }
    }

    pub async fn tick(&mut self, mid: f64) -> Option<SignalResult> {
        if mid <= 0.0 {
            return None;
        }

        self.z_score.update(mid);
        self.rsi.update(mid);
        self.bollinger.update(mid);

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
}
