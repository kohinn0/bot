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
}

impl SignalEngine {
    pub fn new(config: StrategyConfig, state_ref: Arc<RwLock<L2BookState>>) -> Self {
        Self {
            config,
            state_ref,
            price_history: VecDeque::new(),
            history_limit: 60, // pl. megőrzünk 60 tick-et mozgóátlaghoz
        }
    }

    /// Extrém gyors tick-szintű non-blocking olvasás
    pub async fn tick(&mut self) -> Option<SignalResult> {
        let current_state = self.state_ref.read().await.clone();
        
        if current_state.mid_price == 0.0 {
            return None; // Még nem jött be adat
        }

        self.price_history.push_back(current_state.mid_price);
        if self.price_history.len() > self.history_limit {
            self.price_history.pop_front();
        }

        if self.price_history.len() < 10 {
            return None; // Kevés adat a statisztikához
        }

        let mean: f64 = self.price_history.iter().sum::<f64>() / self.price_history.len() as f64;
        let mut variance = 0.0;
        for &price in &self.price_history {
            variance += (price - mean).powi(2);
        }
        variance /= self.price_history.len() as f64;
        let std_dev = variance.sqrt().max(0.0001); // div0 védelem

        let z_score = (current_state.mid_price - mean) / std_dev;

        // "Toxic Flow" / Imbalance logika
        // A threshold-et korrigáljuk a sigma_r (volatilitás szorzó) értékkel
        let base_threshold = self.config.z_score_threshold;
        let volatility_adj_threshold = base_threshold * self.config.sigma_r;
        
        if z_score < -volatility_adj_threshold && current_state.imbalance > 0.6 {
            // Felfelé pattanást várunk -> Long Csapda
            return Some(SignalResult {
                side: "Buy".to_string(),
                target_mid: current_state.mid_price,
            });
        } else if z_score > volatility_adj_threshold && current_state.imbalance < 0.4 {
            // Lefelé pattanást várunk -> Short Csapda
            return Some(SignalResult {
                side: "Sell".to_string(),
                target_mid: current_state.mid_price,
            });
        }

        None
    }
}

pub struct SignalResult {
    pub side: String,
    pub target_mid: f64,
}
