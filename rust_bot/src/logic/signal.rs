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
            prev_imbalance: 0.5,
        }
    }

    /// Nagy teljesítményű, O(1) szórás és momentum alapú szignál generálás
    pub async fn tick(&mut self) -> Option<SignalResult> {
        // 1. MEMÓRIA OPTIMALIZÁLÁS: Csak a szükséges mezőket olvassuk ki, nincs .clone()!
        let (mid_price, imbalance) = {
            let lock = self.state_ref.read().await;
            (lock.mid_price, lock.imbalance)
        };
        
        if mid_price == 0.0 {
            return None;
        }

        // 2. O(1) RUNNING STATS: Frissítjük a futó összegeket
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
            return None; // Minimális minta a stabilitáshoz
        }

        // Statisztikai számítások constant time-ban
        let mean = self.sum_x / n;
        let variance = (self.sum_x2 / n) - (mean * mean);
        let std_dev = variance.max(0.0000001).sqrt(); // div0 elleni védelem
        let z_score = (mid_price - mean) / std_dev;

        // 3. IMBALANCE MOMENTUM: Figyeljük a falak épülésének sebességét
        let imb_momentum = imbalance - self.prev_imbalance;
        self.prev_imbalance = imbalance;

        // Stratégiai paraméterek
        let base_threshold = self.config.z_score_threshold;
        let vol_adj_threshold = base_threshold * self.config.sigma_r;

        // 4. AGRESSZÍV SZIGNÁL LOGIKA (A mentor tanácsa alapján)
        // Nem csak az imbalance szintet nézzük, hanem a momentumot is (ha hirtelen nő a nyomás)
        
        if z_score < -vol_adj_threshold && (imbalance > 0.7 || imb_momentum > 0.15) {
            // Brutális vételi nyomás alakult ki alul -> Long
            return Some(SignalResult {
                side: "Buy".to_string(),
                target_mid: mid_price, 
            });
        } else if z_score > vol_adj_threshold && (imbalance < 0.3 || imb_momentum < -0.15) {
            // Brutális eladói nyomás alakult ki felül -> Short
            return Some(SignalResult {
                side: "Sell".to_string(),
                target_mid: mid_price,
            });
        }

        None
    }
}

pub struct SignalResult {
    pub side: String,
    pub target_mid: f64,
}
