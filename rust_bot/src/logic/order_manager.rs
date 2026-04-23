use crate::config::StrategyConfig;
use serde::Serialize;

#[derive(Serialize, Debug, Clone)]
pub struct LimitOrderType {
    pub tif: String,
}

#[derive(Serialize, Debug, Clone)]
pub struct TriggerOrderType {
    #[serde(rename = "isMarket")]
    pub is_market: bool,
    #[serde(rename = "triggerPx")]
    pub trigger_px: String,
    pub tpsl: String,
}

#[derive(Serialize, Debug, Clone)]
pub struct OrderTypeWire {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<LimitOrderType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<TriggerOrderType>,
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
pub struct CancelWire {
    pub a: u32,
    pub o: u64,
}

#[derive(Serialize, Debug, Clone)]
pub struct CancelAction {
    #[serde(rename = "type")]
    pub type_: String,
    pub cancels: Vec<CancelWire>,
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

    pub fn clamp_tpsl_prices_for_mark(
        exchange_pos: f64,
        tp_price: f64,
        sl_price: f64,
        mark_mid: f64,
        tick: f64,
    ) -> Option<(f64, f64)> {
        if !mark_mid.is_finite() || mark_mid <= 0.0 || !tick.is_finite() || tick <= 0.0 {
            return None;
        }
        // Minimum trigger-távolság a mark-tól: a HL néha csendben (Null) elutasítja
        // a túl szoros trigger-t. 0.3% biztonsági keret + abszolút minimum 2 tick.
        // Korábban csak `tick*2` volt (SOL-nál ≈0.023%), ami a mark ellen futott pozíción
        // a clamp utáni SL-t közvetlenül a mark mellé tette → HL rejected.
        let min_sep = (mark_mid * 0.003).max(tick * 2.0);
        let ceil_tick = |px: f64| (px / tick).ceil() * tick;
        let floor_tick = |px: f64| (px / tick).floor() * tick;

        if exchange_pos > 0.0 {
            let tp = ceil_tick(tp_price.max(mark_mid + min_sep));
            let sl = floor_tick(sl_price.min(mark_mid - min_sep));
            if tp > mark_mid && sl < mark_mid && tp > sl + tick {
                Some((tp, sl))
            } else {
                None
            }
        } else if exchange_pos < 0.0 {
            let tp = floor_tick(tp_price.min(mark_mid - min_sep));
            let sl = ceil_tick(sl_price.max(mark_mid + min_sep));
            if tp < mark_mid && sl > mark_mid && sl > tp + tick {
                Some((tp, sl))
            } else {
                None
            }
        } else {
            None
        }
    }

    pub fn tp_sl_prices_for_position(
        exchange_pos: f64,
        ref_px: f64,
        vol_px: f64,
        min_tick: f64,
        tp_min_pct: f64,
        sl_min_pct: f64,
        mark_mid: f64,
        maker_fee_rate: f64,
        taker_fee_rate: f64,
    ) -> Option<(f64, f64)> {
        let round_trip_fee_px = ref_px * (maker_fee_rate + taker_fee_rate);
        let fee_min_dist = round_trip_fee_px * 2.0;
        let config_min_dist = ref_px * (tp_min_pct / 100.0);
        let tp_dist = f64::max(vol_px * 2.5, f64::max(config_min_dist, fee_min_dist));
        let sl_min_dist = ref_px * (sl_min_pct / 100.0);
        let sl_dist = f64::max(vol_px * 1.5, sl_min_dist);

        let raw_tp = if exchange_pos > 0.0 { ref_px + tp_dist } else { ref_px - tp_dist };
        let raw_sl = if exchange_pos > 0.0 { ref_px - sl_dist } else { ref_px + sl_dist };

        Self::clamp_tpsl_prices_for_mark(exchange_pos, raw_tp, raw_sl, mark_mid, min_tick)
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

    pub fn build_cancel_payload(&self, oids: &[u64]) -> CancelAction {
        let cancels = oids.iter().map(|oid| CancelWire { a: self.asset_idx, o: *oid }).collect();
        CancelAction { type_: "cancel".to_string(), cancels }
    }

    pub fn build_leverage_payload(&self) -> UpdateLeverageAction {
        UpdateLeverageAction {
            type_: "updateLeverage".to_string(),
            asset: self.asset_idx,
            is_cross: !self.config.is_isolated,
            leverage: self.config.leverage,
        }
    }

    pub fn build_protective_tpsl_payload(
        &self,
        side: &str,
        tp_price: f64,
        sl_price: f64,
        sz: f64,
    ) -> OrderAction {
        let is_buy = side.to_lowercase() == "buy";
        let orders = vec![
            OrderWire {
                a: self.asset_idx,
                b: is_buy,
                p: Self::float_to_wire(tp_price),
                s: Self::float_to_wire(sz),
                r: true,
                t: OrderTypeWire {
                    limit: None,
                    trigger: Some(TriggerOrderType {
                        is_market: true,
                        trigger_px: Self::float_to_wire(tp_price),
                        tpsl: "tp".to_string(),
                    }),
                },
            },
            OrderWire {
                a: self.asset_idx,
                b: is_buy,
                p: Self::float_to_wire(sl_price),
                s: Self::float_to_wire(sz),
                r: true,
                t: OrderTypeWire {
                    limit: None,
                    trigger: Some(TriggerOrderType {
                        is_market: true,
                        trigger_px: Self::float_to_wire(sl_price),
                        tpsl: "sl".to_string(),
                    }),
                },
            },
        ];
        OrderAction { type_: "order".to_string(), orders, grouping: "positionTpsl".to_string() }
    }

    /// Csak **belépő** maker létra (`reduceOnly=false`). Pozíciózárást ne itt — TP/SL + dust IOC,
    /// különben „fantom” reduce-only Alo limit jelenhet meg a TP/SL mellé.
    pub fn build_ladder_payload(
        &self,
        side: &str,
        mid_price: f64,
        best_bid: f64,
        best_ask: f64,
        sz_usd: f64,
    ) -> OrderAction {
        self.build_ladder_payload_with_passive_buffer(
            side, mid_price, best_bid, best_ask, sz_usd, 1.0,
        )
    }

    pub fn build_ladder_payload_with_passive_buffer(
        &self,
        side: &str,
        mid_price: f64,
        best_bid: f64,
        best_ask: f64,
        sz_usd: f64,
        passive_buffer_ticks: f64,
    ) -> OrderAction {
        let is_buy = side.to_lowercase() == "buy";
        let tick = self.config.min_tick_size;
        let sz_step = 10_f64.powi(-(self.sz_decimals as i32));

        let mut orders = Vec::new();

        for level_cfg in &self.config.ladder_levels {
            let skew_adj_px = (self.current_pos * self.config.skew_penalty.unwrap_or(0.0)) * tick;
            let base_offset_px = mid_price * (level_cfg.offset_from_mid_pct / 100.0);

            let mut raw_price: f64 = if is_buy {
                mid_price - base_offset_px - skew_adj_px
            } else {
                mid_price + base_offset_px + skew_adj_px
            };

            if level_cfg.level == 1 {
                if is_buy {
                    raw_price = raw_price.max(best_bid).min(best_ask - 0.5 * tick);
                } else {
                    raw_price = raw_price.min(best_ask).max(best_bid + 0.5 * tick);
                }
            }

            let mut rounded_price = if is_buy {
                (raw_price / tick).floor() * tick
            } else {
                (raw_price / tick).ceil() * tick
            };
            if is_buy {
                rounded_price = rounded_price.min(best_bid - passive_buffer_ticks * tick);
            } else {
                rounded_price = rounded_price.max(best_ask + passive_buffer_ticks * tick);
            }

            let size_usd = sz_usd * level_cfg.size_pct;
            let sz = ((size_usd / rounded_price) / sz_step).floor() * sz_step;

            let min_n = self.config.min_ladder_order_notional_usd;
            if sz < self.config.min_shares || (rounded_price * sz) < min_n {
                continue;
            }

            orders.push(OrderWire {
                a: self.asset_idx,
                b: is_buy,
                p: Self::float_to_wire(rounded_price),
                s: Self::float_to_wire(sz),
                r: false,
                t: OrderTypeWire {
                    limit: Some(LimitOrderType { tif: "Alo".to_string() }),
                    trigger: None,
                },
            });
        }

        if orders.is_empty() {
            let tick_price = if is_buy {
                best_bid.max(mid_price - passive_buffer_ticks * tick)
            } else {
                best_ask.min(mid_price + passive_buffer_ticks * tick)
            };
            let rounded_price = if is_buy {
                (tick_price / tick).floor() * tick
            } else {
                (tick_price / tick).ceil() * tick
            };
            let min_notional = self.config.min_ladder_order_notional_usd;
            let fallback_notional = sz_usd.max(min_notional);
            let sz = ((fallback_notional / rounded_price) / sz_step).floor() * sz_step;
            if sz >= self.config.min_shares && (rounded_price * sz) >= min_notional {
                orders.push(OrderWire {
                    a: self.asset_idx,
                    b: is_buy,
                    p: Self::float_to_wire(rounded_price),
                    s: Self::float_to_wire(sz),
                    r: false,
                    t: OrderTypeWire {
                        limit: Some(LimitOrderType { tif: "Alo".to_string() }),
                        trigger: None,
                    },
                });
            }
        }

        OrderAction { type_: "order".to_string(), orders, grouping: "na".to_string() }
    }

    /// IOC limit: azonnali kitöltéshez a limitnek a könyv **szélén** kell lennie.
    /// Sell (long zárás): `price` ≈ **best bid**; Buy (short zárás): `price` ≈ **best ask** (ne entry / mid).
    pub fn build_market_close_payload(
        &self,
        is_buy: bool,
        price: f64,
        sz: f64,
    ) -> OrderAction {
        let sz_step = 10_f64.powi(-(self.sz_decimals as i32));
        let floored = (sz / sz_step).floor() * sz_step;
        let mut quantized_sz = floored.min(sz);
        if quantized_sz < self.config.min_shares && sz + 1e-12 >= self.config.min_shares {
            quantized_sz = self.config.min_shares.min(sz);
        }
        let rounded_px = if is_buy {
            (price / self.config.min_tick_size).ceil() * self.config.min_tick_size
        } else {
            (price / self.config.min_tick_size).floor() * self.config.min_tick_size
        };

        OrderAction {
            type_: "order".to_string(),
            orders: vec![OrderWire {
                a: self.asset_idx,
                b: is_buy,
                p: Self::float_to_wire(rounded_px),
                s: Self::float_to_wire(quantized_sz),
                r: true,
                t: OrderTypeWire {
                    limit: Some(LimitOrderType {
                        tif: "Ioc".to_string(),
                    }),
                    trigger: None,
                },
            }],
            grouping: "na".to_string(),
        }
    }
}

#[cfg(test)]
mod tpsl_pct_tests {
    use super::OrderManager;

    #[test]
    fn tp_sl_minimum_distance_follows_pct_of_ref() {
        let ref_px = 200.0;
        let mark = ref_px;
        let vol = 1e-9;
        let (tp, sl) = OrderManager::tp_sl_prices_for_position(
            1.0,
            ref_px,
            vol,
            0.001,
            1.0,
            1.25,
            mark,
            0.00015,
            0.00045,
        )
        .expect("tpsl");
        let tp_dist = tp - ref_px;
        let sl_dist = ref_px - sl;
        assert!((tp_dist - 2.0).abs() < 0.05, "tp_dist={}", tp_dist);
        assert!((sl_dist - 2.5).abs() < 0.05, "sl_dist={}", sl_dist);
    }
}

