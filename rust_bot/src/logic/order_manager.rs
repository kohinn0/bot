use crate::config::StrategyConfig;
use serde_json::{json, Value};

pub struct OrderManager {
    config: StrategyConfig,
}

impl OrderManager {
    pub fn new(config: StrategyConfig) -> Self {
        Self { config }
    }

    /// Létrehozza a 3-szintes limit (Maker) rendelés batch payloadját az ambushoz
    pub fn build_ladder_payload(
        &self,
        side: &str,
        mid_price: f64,
        sz_usd: f64,
    ) -> Value {
        let is_buy = side.to_lowercase() == "buy";
        let sign = if is_buy { -1.0 } else { 1.0 };

        let sigma = self.config.sigma_r * self.config.skew_penalty.unwrap_or(1.0);
        let tick = self.config.min_tick_size;

        let mut orders = Vec::new();
        let levels = 3;
        
        for i in 1..=levels {
            let limit_price = mid_price * (1.0 + (sign * sigma * i as f64));
            
            // Kerekítés tick size-ra
            let rounded_price = (limit_price / tick).round() * tick;
            let sz = sz_usd / rounded_price;

            orders.push(json!({
                "a": 0, // Asset index, TODO: dinamikusan lekérni
                "b": is_buy,
                "p": format!("{:.4}", rounded_price), // Price
                "s": format!("{:.4}", sz),            // Size
                "r": false,                           // Reduce only
                "t": {"limit": {"tif": "Alo"}},       // ALO = Add Liquidity Only (Post-Only Maker)
            }));
        }

        json!({
            "action": {
                "type": "order",
                "orders": orders,
                "grouping": "na"
            },
            "nonce": 0 // TODO: Ezt a signer állítja be aláírás előtt
        })
    }
}
