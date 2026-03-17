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
    pub current_pos: f64, // Új: pillanatnyi pozíció követése a skew-hoz
}

impl OrderManager {
    pub fn new(config: StrategyConfig, asset_idx: u32, sz_decimals: u32) -> Self {
        Self { config, asset_idx, sz_decimals, current_pos: 0.0 }
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

    /// Létrehozza a Take Profit (Exit) megbízást az azonnali hurokhoz
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
                    tif: "Gtc".to_string(), // Exitnél használjunk GTC-t hogy biztosan bent maradjon
                }
            }
        });

        OrderAction {
            type_: "order".to_string(),
            orders,
            grouping: "na".to_string(),
        }
    }

    /// ÚJ: Dinamikus árazás a Best Bid/Ask figyelembevételével
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
            
            let mut raw_price = if is_buy {
                mid_price - (offset_ticks * tick)
            } else {
                mid_price + (offset_ticks * tick)
            };

            // 💡 MENTOR TRÜKK: Az 1. szinten próbáljunk "Join"-olni vagy 1 tickkel agresszívabbak lenni
            if level_cfg.level == 1 {
                raw_price = if is_buy {
                    raw_price.max(best_bid + tick) // Legyünk 1 tickkel a legjobb vételi felett
                } else {
                    raw_price.min(best_ask - tick) // Legyünk 1 tickkel a legjobb eladási alatt
                };
            }
            
            let rounded_price = (raw_price / tick).round() * tick;
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
