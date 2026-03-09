use crate::config::StrategyConfig;
use serde::Serialize;

#[derive(Serialize, Debug, Clone)]
pub struct LimitOrderType {
    pub tif: String,
}

#[derive(Serialize, Debug, Clone)]
pub struct OrderTypeWire {
    pub limit: LimitOrderType,
}

#[derive(Serialize, Debug, Clone)]
pub struct OrderWire {
    pub a: u32,
    pub b: bool,
    pub p: String,
    pub s: String,
    pub r: bool,
    pub t: OrderTypeWire,
}

#[derive(Serialize, Debug, Clone)]
pub struct OrderAction {
    #[serde(rename = "type")]
    pub type_: String,
    pub orders: Vec<OrderWire>,
    pub grouping: String,
}

pub struct OrderManager {
    config: StrategyConfig,
    asset_idx: u32,
    sz_decimals: u32,
}

impl OrderManager {
    pub fn new(config: StrategyConfig, asset_idx: u32, sz_decimals: u32) -> Self {
        Self { config, asset_idx, sz_decimals }
    }

    /// Hyperliquid float serialization rules: max 8 decimals, no trailing zeroes, no trailing decimal points.
    fn float_to_wire(x: f64) -> String {
        let s = format!("{:.8}", x);
        let trimmed = s.trim_end_matches('0');
        if trimmed.ends_with('.') {
            trimmed.trim_end_matches('.').to_string()
        } else {
            trimmed.to_string()
        }
    }

    /// Létrehozza a 3-szintes limit (Maker) rendelés array-t egy szigorú típusú hierarchiában
    pub fn build_ladder_payload(
        &self,
        side: &str,
        mid_price: f64,
        sz_usd: f64,
    ) -> OrderAction {
        let is_buy = side.to_lowercase() == "buy";
        let tick = self.config.min_tick_size;
        let sz_step = 10_f64.powi(-(self.sz_decimals as i32));

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
            
            let rounded_price = (raw_price / tick).round() * tick;
            
            let size_usd = sz_usd * level_cfg.size_pct;
            let raw_sz = size_usd / rounded_price;
            let sz = (raw_sz / sz_step).floor() * sz_step;

            // Hyperliquid also places a hard minimum on order size value ($10)
            // But for safety, we round aggressively down.
            orders.push(OrderWire {
                a: self.asset_idx,
                b: is_buy,
                p: Self::float_to_wire(rounded_price),
                s: Self::float_to_wire(sz),
                r: false,
                t: OrderTypeWire {
                    limit: LimitOrderType {
                        tif: "Alo".to_string(),
                    }
                }
            });
        }

        OrderAction {
            type_: "order".to_string(),
            orders,
            grouping: "na".to_string(),
        }
    }
}
