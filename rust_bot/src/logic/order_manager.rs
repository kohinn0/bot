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
}

impl OrderManager {
    pub fn new(config: StrategyConfig) -> Self {
        Self { config }
    }

    /// Segédfüggvény a tizedesjegyek számolásához f64-nél
    fn count_decimals(value: f64) -> usize {
        let s = format!("{:.10}", value);
        let trimmed = s.trim_end_matches('0');
        if let Some(pos) = trimmed.find('.') {
            trimmed.len() - pos - 1
        } else {
            0
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
        let min_shares = self.config.min_shares;

        let price_decimals = Self::count_decimals(tick);
        let size_decimals = Self::count_decimals(min_shares);

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
            let sz = (raw_sz / min_shares).floor() * min_shares;

            orders.push(OrderWire {
                a: 0,
                b: is_buy,
                p: format!("{:.*}", price_decimals, rounded_price),
                s: format!("{:.*}", size_decimals, sz),
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
