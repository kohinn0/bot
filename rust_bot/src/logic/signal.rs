use crate::config::StrategyConfig;
use std::collections::VecDeque;
use std::time::Instant;

pub struct SignalResult {
    pub side: String,
    pub target_mid: f64,
    pub volatility: f64,
}

pub trait Indicator {
    fn update(&mut self, price: f64);
    fn evaluate(&self) -> Option<String>;
}

// ==========================================
// 1. Z-SCORE INDIKÁTOR (A Villámgyors Ravasz)
// ==========================================
struct ZScoreIndicator {
    config: crate::config::ZScoreConfig,
    history: VecDeque<f64>,
}
impl ZScoreIndicator {
    fn new(config: crate::config::ZScoreConfig) -> Self { Self { config, history: VecDeque::new() } }
}
impl Indicator for ZScoreIndicator {
    fn update(&mut self, price: f64) {
        self.history.push_back(price);
        if self.history.len() > self.config.window { self.history.pop_front(); }
    }
    fn evaluate(&self) -> Option<String> {
        if !self.config.enabled || self.history.len() < self.config.window { return None; }
        let mean: f64 = self.history.iter().sum::<f64>() / self.history.len() as f64;
        let variance: f64 = self.history.iter().map(|&x| (x - mean).powi(2)).sum::<f64>() / self.history.len() as f64;
        let std_dev = variance.sqrt();
        if std_dev == 0.0 { return None; }
        let z_score = (*self.history.back().unwrap() - mean) / std_dev;
        
        if z_score <= -self.config.threshold { Some("Buy".to_string()) }
        else if z_score >= self.config.threshold { Some("Sell".to_string()) }
        else { None }
    }
}

// ==========================================
// 2. RSI INDIKÁTOR (A Trend Környezet)
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
            last_update: Instant::now() - std::time::Duration::from_secs(100), // Azonnali első frissítés
        } 
    }
}
impl Indicator for RsiIndicator {
    fn update(&mut self, price: f64) {
        // Okos mintavételezés: csak 5 másodpercenként rögzítünk árat, hogy elkerüljük a mikroszekundumos zajt
        if self.last_update.elapsed().as_secs() < 5 { return; }
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
                // Wilder's Smoothing
                self.avg_gain = ((self.avg_gain * (self.config.window as f64 - 1.0)) + gain) / self.config.window as f64;
                self.avg_loss = ((self.avg_loss * (self.config.window as f64 - 1.0)) + loss) / self.config.window as f64;
            }
        }
        self.prev_price = Some(price);
    }

    fn evaluate(&self) -> Option<String> {
        if !self.config.enabled || self.tick_count < self.config.window { return None; }
        if self.avg_loss == 0.0 {
            return if self.avg_gain > 0.0 { Some("Sell".to_string()) } else { None };
        }
        let rs = self.avg_gain / self.avg_loss;
        let rsi = 100.0 - (100.0 / (1.0 + rs));

        if rsi <= self.config.buy_below { Some("Buy".to_string()) }
        else if rsi >= self.config.sell_above { Some("Sell".to_string()) }
        else { None }
    }
}

// ==========================================
// A MOTOR (Konfluencia Logika)
// ==========================================
pub struct SignalEngine {
    config: StrategyConfig,
    z_score: ZScoreIndicator,
    rsi: RsiIndicator,
}

impl SignalEngine {
    pub fn new(config: StrategyConfig) -> Self {
        Self {
            z_score: ZScoreIndicator::new(config.signals.z_score.clone()),
            rsi: RsiIndicator::new(config.signals.rsi.clone()),
            config,
        }
    }

    pub async fn tick(&mut self, mid: f64) -> Option<SignalResult> {
        if mid <= 0.0 { return None; }

        self.z_score.update(mid);
        self.rsi.update(mid);

        let z_sig = self.z_score.evaluate();
        let rsi_sig = self.rsi.evaluate();

        let z_enabled = self.config.signals.z_score.enabled;
        let rsi_enabled = self.config.signals.rsi.enabled;

        let mut final_sig = None;

        // KŐKEMÉNY LOGIKA:
        // Ha mindkettő be van kapcsolva, mindkettőnek EGYEZNIE KELL.
        if z_enabled && rsi_enabled {
            if let (Some(z), Some(r)) = (&z_sig, &rsi_sig) {
                if z == r { final_sig = Some(z.clone()); }
            }
        } 
        // Ha csak az egyik van bekapcsolva, az diktál.
        else if z_enabled { final_sig = z_sig; } 
        else if rsi_enabled { final_sig = rsi_sig; }

        if let Some(side) = final_sig {
            return Some(SignalResult {
                side,
                target_mid: mid,
                volatility: mid * 0.005, 
            });
        }
        None
    }
}
