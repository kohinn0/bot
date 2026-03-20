use crate::config::StrategyConfig;
use crate::network::feed::L2BookState;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct SignalEngine {
    config: StrategyConfig,
    state_ref: Arc<RwLock<L2BookState>>,
    return_history: VecDeque<f64>,
    history_limit: usize,
    
    // O(1) running stats
    sum_x: f64,
    sum_x2: f64,
    
    // Stability tracking
    tick_count: u64,
    
    // Imbalance tracking
    prev_imbalance: f64,
    prev_mid_price: Option<f64>,
}

impl SignalEngine {
    pub fn new(config: StrategyConfig, state_ref: Arc<RwLock<L2BookState>>) -> Self {
        Self {
            config,
            state_ref,
            return_history: VecDeque::new(),
            history_limit: 500, // 200-ról 500-ra emelve a stabilitásért
            sum_x: 0.0,
            sum_x2: 0.0,
            tick_count: 0,
            prev_imbalance: 0.5,
            prev_mid_price: None,
        }
    }

    /// Matematikailag stabil, HFT-optimalizált szignál generátor
    pub async fn tick(&mut self) -> Option<SignalResult> {
        // 1. Gyors, non-allocating adat lekérés
        let (mid_price, imbalance, ts) = {
            let lock = self.state_ref.read().await;
            (lock.mid_price, lock.imbalance, lock.last_update_ts)
        };
        
        // STALENESS CHECK: Ha az adat régebbi mint 500ms, eldobjuk (Latency védelem)
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        if now - ts > 500 {
            return None;
        }
        
        if mid_price == 0.0 {
            return None;
        }

        self.tick_count += 1;

        // 2. STABILITÁS: Lebegőpontos hiba (Drift) kezelése
        // 1000 tickenként újraszámoljuk a teljes összeget a VecDeque-ből,
        // hogy elkerüljük a folyamatos hozzáadás/kivonás okozta pontatlanságot.
        if self.tick_count % 1000 == 0 {
            self.sum_x = self.return_history.iter().sum::<f64>();
            self.sum_x2 = self.return_history.iter().map(|&x| x * x).sum::<f64>();
        }

        // Return-based signal: numerically more stable across price regimes.
        let prev_mid = if let Some(v) = self.prev_mid_price {
            v
        } else {
            self.prev_mid_price = Some(mid_price);
            return None;
        };
        self.prev_mid_price = Some(mid_price);
        if prev_mid <= 0.0 || mid_price <= 0.0 {
            return None;
        }
        let log_ret = (mid_price / prev_mid).ln();

        // Running update logic on returns
        if self.return_history.len() >= self.history_limit {
            if let Some(old_ret) = self.return_history.pop_front() {
                self.sum_x -= old_ret;
                self.sum_x2 -= old_ret * old_ret;
            }
        }
        
        self.return_history.push_back(log_ret);
        self.sum_x += log_ret;
        self.sum_x2 += log_ret * log_ret;

        let n = self.return_history.len() as f64;
        if n < 15.0 {
            return None;
        }

        // Variancia és Z-Score (numerikusan biztonságos formában)
        let mean = self.sum_x / n;
        let variance = ((self.sum_x2 / n) - (mean * mean)).max(0.0); // Nincs negatív variancia!
        let std_dev = variance.sqrt().max(0.0000001);
        let z_score = (log_ret - mean) / std_dev;
        let volatility_px = (std_dev * mid_price).abs();

        // 3. IMBALANCE MOMENTUM (A mentor kedvence)
        let imb_momentum = imbalance - self.prev_imbalance;
        self.prev_imbalance = imbalance;

        // Stratégiai paraméterek
        let base_threshold = self.config.z_score_threshold; // Most már a config-ból olvassuk!
        let vol_adj_threshold = base_threshold * self.config.sigma_r;

        // 4. "HÚSEVŐ" HFT ÁRAZÁS
        // Ha nagy a vételi nyomás (Imbalance + Momentum), ne csak a Mid-re várjunk,
        // hanem próbáljunk agresszívabban "ráülni" a Bid falra.
        
        if z_score < -vol_adj_threshold && (imbalance > 0.72 || imb_momentum > 0.12) {
            return Some(SignalResult {
                side: "Buy".to_string(),
                target_mid: mid_price, // Visszaállítva Mid-re, az eltolást az OrderManager intézi a spread-en belülre
                volatility: volatility_px,
            });
        } else if z_score > vol_adj_threshold && (imbalance < 0.28 || imb_momentum < -0.12) {
            return Some(SignalResult {
                side: "Sell".to_string(),
                target_mid: mid_price,
                volatility: volatility_px,
            });
        }

        None
    }
}

pub struct SignalResult {
    pub side: String,
    pub target_mid: f64,
    pub volatility: f64,
}
