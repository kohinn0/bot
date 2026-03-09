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
        let tick = self.config.min_tick_size;

        let mut orders = Vec::new();
        
        let ticks_in_mid = (mid_price / tick).floor();
        let base_price = ticks_in_mid * tick;
        
        for level_cfg in &self.config.ladder_levels {
            let offset_ticks = level_cfg.offset_from_mid_ticks as f64;
            
            let raw_price = if is_buy {
                base_price + (offset_ticks * tick)
            } else {
                base_price + (-offset_ticks * tick)
            };
            
            // Kerekítés tick size-ra
            let rounded_price = (raw_price / tick).round() * tick;
            
            // Ekkora dollárértéket akarunk ezen a szinten venni
            let size_usd = sz_usd * level_cfg.size_pct;
            let raw_sz = size_usd / rounded_price;
            
            // Rounding size via flooring to min_shares
            let min_shares = self.config.min_shares;
            let sz = (raw_sz / min_shares).floor() * min_shares;

            orders.push(json!({
                "a": 0, // Asset index, TODO: dinamikusan lekérni a meta endpointról
                "b": is_buy,
                "p": format!("{:.4}", rounded_price), // Price
                "s": format!("{:.*}", 3, sz),            // Size (mennyiség), SOL 3 dec.
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
