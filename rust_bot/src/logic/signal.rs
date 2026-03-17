use crate::config::StrategyConfig;
use crate::network::feed::L2BookState;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct SignalEngine {
    config: StrategyConfig,
    state_ref: Arc<RwLock<L2BookState>>,
    price_history: VecDeque<f64>,
    history_limit: usize,
    
    // O(1) running stats
    sum_x: f64,
    sum_x2: f64,
    
    // Stability tracking
    tick_count: u64,
    
    // Imbalance tracking
    prev_imbalance: f64,
}

impl SignalEngine {
    pub fn new(config: StrategyConfig, state_ref: Arc<RwLock<L2BookState>>) -> Self {
        Self {
            config,
            state_ref,
            price_history: VecDeque::new(),
            history_limit: 60,
            sum_x: 0.0,
            sum_x2: 0.0,
            tick_count: 0,
            prev_imbalance: 0.5,
        }
    }

    /// Matematikailag stabil, HFT-optimalizált szignál generátor
    pub async fn tick(&mut self) -> Option<SignalResult> {
        // 1. Gyors, non-allocating adat lekérés
        let (mid_price, imbalance) = {
            let lock = self.state_ref.read().await;
            (lock.mid_price, lock.imbalance)
        };
        
        if mid_price == 0.0 {
            return None;
        }

        self.tick_count += 1;

        // 2. STABILITÁS: Lebegőpontos hiba (Drift) kezelése
        // 1000 tickenként újraszámoljuk a teljes összeget a VecDeque-ből,
        // hogy elkerüljük a folyamatos hozzáadás/kivonás okozta pontatlanságot.
        if self.tick_count % 1000 == 0 {
            self.sum_x = self.price_history.iter().sum::<f64>();
            self.sum_x2 = self.price_history.iter().map(|&x| x * x).sum::<f64>();
        }

        // Running update logic
        if self.price_history.len() >= self.history_limit {
            if let Some(old_price) = self.price_history.pop_front() {
                self.sum_x -= old_price;
                self.sum_x2 -= old_price * old_price;
            }
        }
        
        self.price_history.push_back(mid_price);
        self.sum_x += mid_price;
        self.sum_x2 += mid_price * mid_price;

        let n = self.price_history.len() as f64;
        if n < 15.0 {
            return None;
        }

        // Variancia és Z-Score (numerikusan biztonságos formában)
        let mean = self.sum_x / n;
        let variance = ((self.sum_x2 / n) - (mean * mean)).max(0.0); // Nincs negatív variancia!
        let std_dev = variance.sqrt().max(0.0000001);
        let z_score = (mid_price - mean) / std_dev;

        // 3. IMBALANCE MOMENTUM (A mentor kedvence)
        let imb_momentum = imbalance - self.prev_imbalance;
        self.prev_imbalance = imbalance;

        let base_threshold = self.config.z_score_threshold;
        let vol_adj_threshold = base_threshold * self.config.sigma_r;

        // 4. "HÚSEVŐ" HFT ÁRAZÁS
        // Ha nagy a vételi nyomás (Imbalance + Momentum), ne csak a Mid-re várjunk,
        // hanem próbáljunk agresszívabban "ráülni" a Bid falra.
        
        if z_score < -vol_adj_threshold && (imbalance > 0.72 || imb_momentum > 0.12) {
            return Some(SignalResult {
                side: "Buy".to_string(),
                // 💡 AGRESSZÍV ELTOLÁS: 1 tick-es mozgást előrejelzünk
                target_mid: mid_price + self.config.min_tick_size, 
            });
        } else if z_score > vol_adj_threshold && (imbalance < 0.28 || imb_momentum < -0.12) {
            return Some(SignalResult {
                side: "Sell".to_string(),
                target_mid: mid_price - self.config.min_tick_size,
            });
        }

        None
    }
}

pub struct SignalResult {
    pub side: String,
    pub target_mid: f64,
}
