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

#[derive(Serialize, Debug, Clone)]
pub struct CancelByCoinAction {
    #[serde(rename = "type")]
    pub type_: String,
    pub coin: String,
}

#[derive(Serialize, Debug, Clone)]
pub struct UpdateLeverageAction {
    #[serde(rename = "type")]
    pub type_: String,
    pub asset: u32,
    #[serde(rename = "isCross")]
    pub is_cross: bool,
    pub leverage: u32,
}

pub struct OrderManager {
    config: StrategyConfig,
    asset_idx: u32,
    sz_decimals: u32,
    pub current_pos: f64,
}

impl OrderManager {
    pub fn new(config: StrategyConfig, asset_idx: u32, sz_decimals: u32) -> Self {
        Self { config, asset_idx, sz_decimals, current_pos: 0.0 }
    }

    fn float_to_wire(x: f64) -> String {
        let s = format!("{:.8}", x);
        let trimmed = s.trim_end_matches('0');
        if trimmed.ends_with('.') {
            trimmed.trim_end_matches('.').to_string()
        } else {
            trimmed.to_string()
        }
    }

    pub fn build_cancel_all_payload(&self) -> CancelByCoinAction {
        CancelByCoinAction {
            type_: "cancelByCoin".to_string(),
            coin: self.config.coin.clone(),
        }
    }

    pub fn build_leverage_payload(&self) -> UpdateLeverageAction {
        UpdateLeverageAction {
            type_: "updateLeverage".to_string(),
            asset: self.asset_idx,
            is_cross: !self.config.is_isolated, 
            leverage: self.config.leverage as u32,
        }
    }

    pub fn build_exit_payload(&self, side: &str, price: f64, sz: f64) -> OrderAction {
        let is_buy = side.to_lowercase() == "buy";
        let mut orders = Vec::new();
        
        orders.push(OrderWire {
            a: self.asset_idx,
            b: is_buy,
            p: Self::float_to_wire(price),
            s: Self::float_to_wire(sz),
            r: false,
            t: OrderTypeWire {
                limit: LimitOrderType {
                    tif: "Gtc".to_string(),
                }
            }
        });

        OrderAction {
            type_: "order".to_string(),
            orders,
            grouping: "na".to_string(),
        }
    }

    pub fn build_ladder_payload(
        &self,
        side: &str,
        mid_price: f64,
        best_bid: f64,
        best_ask: f64,
        sz_usd: f64,
    ) -> OrderAction {
        let is_buy = side.to_lowercase() == "buy";
        let tick = self.config.min_tick_size;
        let sz_step = 10_f64.powi(-(self.sz_decimals as i32));

        let mut orders = Vec::new();
        
        for level_cfg in &self.config.ladder_levels {
            let skew_adj = self.current_pos * self.config.skew_penalty.unwrap_or(0.0);
            let offset_ticks = (level_cfg.offset_from_mid_ticks as f64) + if is_buy { skew_adj } else { -skew_adj };
            
            let abs_offset = offset_ticks.abs();
            let mut raw_price = if is_buy {
                mid_price - (abs_offset * tick)
            } else {
                mid_price + (abs_offset * tick)
            };

            if level_cfg.level == 1 {
                if is_buy {
                    raw_price = raw_price.max(best_bid).min(best_ask - (0.5 * tick));
                } else {
                    raw_price = raw_price.min(best_ask).max(best_bid + (0.5 * tick));
                }
            }
            
            let rounded_price = if is_buy {
                (raw_price / tick).floor() * tick
            } else {
                (raw_price / tick).ceil() * tick
            };
            let size_usd = sz_usd * level_cfg.size_pct;
            let sz = ((size_usd / rounded_price) / sz_step).floor() * sz_step;

            if sz < self.config.min_shares || (rounded_price * sz) < 10.0 {
                continue;
            }

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
