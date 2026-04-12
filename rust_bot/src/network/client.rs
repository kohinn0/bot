use reqwest::Client;
use serde_json::{json, Value};
use std::collections::HashSet;
use ethers::core::types::Signature;

#[derive(Clone)]
pub struct HyperliquidClient {
    pub rest_client: Client,
    pub info_url: String,
    pub exchange_url: String,
    perp_dex: Option<String>,
}

impl HyperliquidClient {
    pub fn new(is_mainnet: bool, perp_dex: Option<String>) -> Self {
        let client = Client::builder()
            .pool_idle_timeout(None)
            .tcp_nodelay(true)
            .build()
            .unwrap();

        let base = if is_mainnet {
            "https://api.hyperliquid.xyz"
        } else {
            "https://api.hyperliquid-testnet.xyz"
        };

        Self {
            rest_client: client,
            info_url: format!("{}/info", base),
            exchange_url: format!("{}/exchange", base),
            perp_dex,
        }
    }

    fn clearinghouse_state_payload(&self, user: &str) -> Value {
        let user = user.trim();
        match &self.perp_dex {
            Some(d) if !d.is_empty() => json!({
                "type": "clearinghouseState",
                "user": user,
                "dex": d.as_str(),
            }),
            _ => json!({
                "type": "clearinghouseState",
                "user": user,
            }),
        }
    }

    pub async fn get_meta(&self) -> Result<Value, reqwest::Error> {
        let payload = json!({"type": "meta"});
        self.rest_client.post(&self.info_url).json(&payload).send().await?.json().await
    }

    pub async fn get_user_state(&self, user: &str) -> Result<Value, reqwest::Error> {
        let payload = self.clearinghouse_state_payload(user);
        self.rest_client.post(&self.info_url).json(&payload).send().await?.json().await
    }

    pub async fn get_spot_clearinghouse_state(&self, user: &str) -> Result<Value, reqwest::Error> {
        let payload = json!({"type": "spotClearinghouseState", "user": user.trim()});
        self.rest_client.post(&self.info_url).json(&payload).send().await?.json().await
    }

    pub async fn get_account_value_usd(&self, user: &str) -> Result<f64, String> {
        let user = user.trim();
        let perp_state = self.get_user_state(user).await
            .map_err(|e| format!("clearinghouseState HTTP: {}", e))?;

        if let Some(err) = perp_state.get("error") {
            return Err(format!("clearinghouseState API hiba: {}", err));
        }

        let perp_usd = parse_clearinghouse_account_value_usd(&perp_state).unwrap_or(0.0);

        let spot_state = match self.get_spot_clearinghouse_state(user).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("spotClearinghouseState HTTP: {}", e);
                json!({})
            }
        };
        let spot_usd = if spot_state.get("error").is_some() {
            tracing::warn!("spotClearinghouseState: {:?}", spot_state.get("error"));
            0.0
        } else {
            parse_spot_usdc_total_usd(&spot_state)
        };

        let total = perp_usd + spot_usd;
        if total.is_finite() && total > 0.0 {
            return Ok(total);
        }

        let ms_av = perp_state.pointer("/marginSummary/accountValue")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "n/a".to_string());
        let spot_n = spot_state.get("balances")
            .and_then(|b| b.as_array())
            .map(|a| a.len());

        Err(format!(
            "HL egyenleg ~0 USD (perp {:.4} + spot USDC {:.4}). perp marginSummary.accountValue={} | spot sorok: {:?}.",
            perp_usd, spot_usd, ms_av, spot_n
        ))
    }

    

    pub async fn get_frontend_open_orders(&self, user: &str) -> Result<Value, reqwest::Error> {
        let payload = json!({"type": "frontendOpenOrders", "user": user});
        self.rest_client.post(&self.info_url).json(&payload).send().await?.json().await
    }

    pub async fn send_l1_action<T: serde::Serialize>(
        &self,
        action: &T,
        nonce: u64,
        signature: Signature,
    ) -> Result<Value, reqwest::Error> {
        let mut r_bytes = [0u8; 32];
        signature.r.to_big_endian(&mut r_bytes);
        let r_hex = hex::encode(r_bytes);

        let mut s_bytes = [0u8; 32];
        signature.s.to_big_endian(&mut s_bytes);
        let s_hex = hex::encode(s_bytes);

        let v_val = signature.v as u8;
        let v = if v_val < 27 { v_val + 27 } else { v_val };

        let payload = json!({
            "action": action,
            "nonce": nonce,
            "signature": {
                "r": format!("0x{}", r_hex),
                "s": format!("0x{}", s_hex),
                "v": v
            }
        });

        self.rest_client.post(&self.exchange_url).json(&payload).send().await?.json().await
    }
}

pub fn collect_resting_oids_from_exchange_response(body: &Value) -> Vec<u64> {
    let mut out = Vec::new();
    let Some(arr) = body.pointer("/response/data/statuses").and_then(|x| x.as_array()) else {
        return out;
    };
    for st in arr {
        let oid = st.pointer("/resting/oid")
            .and_then(|o| o.as_u64().or_else(|| o.as_str().and_then(|s| s.parse().ok())));
        if let Some(o) = oid {
            out.push(o);
        }
    }
    out
}



pub fn exchange_order_submission_ok(body: &Value) -> bool {
    if body.get("status").and_then(|s| s.as_str()) != Some("ok") {
        return false;
    }
    if let Some(arr) = body.pointer("/response/data/statuses").and_then(|x| x.as_array()) {
        for st in arr {
            if st.get("error").is_some() {
                return false;
            }
        }
    }
    true
}

/// Naplóz + `true` ha HTTP ok és `exchange_order_submission_ok` (order/cancel/leverage válaszok).
pub fn exchange_action_ok_or_warn(ctx: &str, res: &Result<Value, reqwest::Error>) -> bool {
    match res {
        Err(e) => {
            tracing::warn!("{}: HTTP / válasz parse: {}", ctx, e);
            false
        }
        Ok(body) => {
            if exchange_order_submission_ok(body) {
                true
            } else {
                tracing::warn!("{}: HL válasz nem ok: {:?}", ctx, body);
                false
            }
        }
    }
}

/// Cancel-specifikus: `true` ha cancel sikeres VAGY orderek már nem léteznek (idempotens).
/// Null body → "valószínűleg ok" (HL hiccup, de cancel valószínűleg végbement).
pub fn exchange_cancel_ok_or_idempotent(ctx: &str, res: &Result<Value, reqwest::Error>) -> bool {
    match res {
        Err(e) => {
            tracing::warn!("{}: HTTP hiba: {}", ctx, e);
            false
        }
        Ok(body) => {
            if body.is_null() {
                tracing::debug!("{}: Null válasz — cancel valószínűleg sikeres", ctx);
                return true;
            }
            if exchange_order_submission_ok(body) {
                return true;
            }
            if body.get("status").and_then(|s| s.as_str()) == Some("ok") {
                if let Some(arr) = body.pointer("/response/data/statuses").and_then(|x| x.as_array()) {
                    let all_gone = arr.iter().all(|st| {
                        st.get("error")
                            .and_then(|e| e.as_str())
                            .map_or(false, |msg| {
                                msg.contains("already canceled")
                                    || msg.contains("already filled")
                                    || msg.contains("never placed")
                            })
                    });
                    if all_gone {
                        tracing::debug!("{}: idempotens — orderek már nem léteznek", ctx);
                        return true;
                    }
                }
            }
            tracing::warn!("{}: cancel válasz nem ok: {:?}", ctx, body);
            false
        }
    }
}



fn parse_order_oid(ord: &Value) -> Option<u64> {
    ord["oid"].as_u64()
        .or_else(|| ord["oid"].as_str().and_then(|v| v.parse().ok()))
}

fn json_truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|x| x != 0.0).unwrap_or(false),
        Value::String(s) => s == "true" || s == "1",
        _ => false,
    }
}

/// TP/SL és egyéb trigger orderek — ezeket nem szabad „stale létra” / szignál cancellel eltenni.
/// A HL néha nem boolt ad (vagy csak triggerPx / orderType alapján lehet felismerni).
pub fn hl_order_is_protected(ord: &Value) -> bool {
    if json_truthy(&ord["isPositionTpsl"]) || json_truthy(&ord["isTrigger"]) {
        return true;
    }
    if let Some(tc) = ord.get("triggerCondition").and_then(|v| v.as_str()) {
        if !tc.is_empty() {
            return true;
        }
    }
    if let Some(tp) = ord.get("triggerPx") {
        if !tp.is_null() {
            let ok = tp.as_str().map(|s| !s.is_empty() && s != "0").unwrap_or(false)
                || tp.as_f64().map(|x| x.abs() > 1e-12).unwrap_or(false);
            if ok {
                return true;
            }
        }
    }
    if let Some(ot) = ord.get("orderType").and_then(|v| v.as_str()) {
        let o = ot.to_ascii_lowercase();
        if o.contains("take profit")
            || o.contains("stop market")
            || o.contains("stop limit")
            || o.contains("stop loss")
        {
            return true;
        }
    }
    false
}

/// Új maker belépő létra: ne tegyünk rá, ha már van TP/SL/trigger, vagy bármilyen reduce-only order (pl. szellem limit).
pub fn hl_order_blocks_entry_ladder(ord: &Value) -> bool {
    hl_order_is_protected(ord) || json_truthy(&ord["reduceOnly"])
}

pub fn frontend_has_blocking_orders_for_coin(frontend_orders: &Value, coin: &str) -> bool {
    let Some(arr) = frontend_orders.as_array() else {
        return false;
    };
    arr.iter()
        .any(|o| o["coin"].as_str() == Some(coin) && hl_order_blocks_entry_ladder(o))
}

/// Bármilyen nyitott order a coinon (létra-cancel után: ha nem üres, nem küldünk új batch-et — TP/SL API mezők nélkül is).
pub fn frontend_has_any_open_order_for_coin(frontend_orders: &Value, coin: &str) -> bool {
    frontend_orders
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .any(|o| o["coin"].as_str() == Some(coin))
}

fn parse_json_number(v: &Value) -> Option<f64> {
    if let Some(s) = v.as_str() {
        s.parse().ok()
    } else {
        v.as_f64()
    }
}

/// HTTP 200 mellett is lehet `{ "error": "..." }` — ilyenkor ne tekintsünk „üres” állapotnak.
/// A `"error": null` nem hiba (néha a kulcs létezik, érték nélkül).
pub fn clearinghouse_has_error(state: &Value) -> bool {
    match state.get("error") {
        None => false,
        Some(v) if v.is_null() => false,
        Some(_) => true,
    }
}

/// HTTP 200 mellett is lehet `{ "error": ... }` a `frontendOpenOrders` válaszban.
/// A `"error": null` nem hiba.
pub fn frontend_has_error(state: &Value) -> bool {
    match state.get("error") {
        None => false,
        Some(v) if v.is_null() => false,
        Some(_) => true,
    }
}

/// Transziens HL API-hibák ellen `frontendOpenOrders`-nál: retry `{ "error": ... }` vagy HTTP hiba esetén.
pub async fn get_frontend_open_orders_retry_ok(
    client: &HyperliquidClient,
    user: &str,
) -> Result<Value, String> {
    get_frontend_open_orders_retry_ok_with(client, user, 4, 100).await
}

/// Gyors útvonal: kevés próba, rövidebb várakozás (`ladder_gate_flat_ok`).
pub async fn get_frontend_open_orders_retry_ok_fast(
    client: &HyperliquidClient,
    user: &str,
) -> Result<Value, String> {
    get_frontend_open_orders_retry_ok_with(client, user, 2, 80).await
}

async fn get_frontend_open_orders_retry_ok_with(
    client: &HyperliquidClient,
    user: &str,
    attempts: u32,
    delay_ms: u64,
) -> Result<Value, String> {
    let attempts = attempts.min(32);
    for i in 0..attempts {
        if i > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
        }
        match client.get_frontend_open_orders(user).await {
            Ok(st) => {
                if frontend_has_error(&st) {
                    tracing::warn!(
                        "frontendOpenOrders error (próbálkozás {}/{}): {:?}",
                        i + 1,
                        attempts,
                        st.get("error")
                    );
                    continue;
                }
                return Ok(st);
            }
            Err(e) => {
                tracing::warn!(
                    "frontendOpenOrders HTTP (próbálkozás {}/{}): {}",
                    i + 1,
                    attempts,
                    e
                );
            }
        }
    }
    Err(format!(
        "frontendOpenOrders: sikertelen {} próbálkozás után",
        attempts
    ))
}

/// Transziens HL API-hibák ellen: több újrapróbálás (`{ "error": ... }` vagy HTTP hiba).
/// Így a reconcile és a szignál loop nem marad `pos_sim`-mel egy átmeneti error válasz miatt.
pub async fn get_user_state_retry_ok(
    client: &HyperliquidClient,
    user: &str,
) -> Result<Value, String> {
    get_user_state_retry_ok_with(client, user, 4, 100).await
}

/// L1 gate / gyors útvonal: kevesebb próba, rövidebb várakozás.
pub async fn get_user_state_retry_ok_fast(
    client: &HyperliquidClient,
    user: &str,
) -> Result<Value, String> {
    get_user_state_retry_ok_with(client, user, 2, 80).await
}

async fn get_user_state_retry_ok_with(
    client: &HyperliquidClient,
    user: &str,
    attempts: u32,
    delay_ms: u64,
) -> Result<Value, String> {
    let attempts = attempts.min(32);
    for i in 0..attempts {
        if i > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
        }
        match client.get_user_state(user).await {
            Ok(st) => {
                if clearinghouse_has_error(&st) {
                    tracing::warn!(
                        "clearinghouseState error (próbálkozás {}/{}): {:?}",
                        i + 1,
                        attempts,
                        st.get("error")
                    );
                    continue;
                }
                return Ok(st);
            }
            Err(e) => {
                tracing::warn!(
                    "get_user_state HTTP (próbálkozás {}/{}): {}",
                    i + 1,
                    attempts,
                    e
                );
            }
        }
    }
    Err(format!(
        "clearinghouseState: sikertelen {} próbálkozás után",
        attempts
    ))
}

/// Egy coin perp pozíciója: (`szi`, `entryPx`). A HL `szi`-t néha stringgel, néha számmal adja.
pub fn clearinghouse_position_for_coin(state: &Value, coin: &str) -> (f64, Option<f64>) {
    let Some(arr) = state.get("assetPositions").and_then(|x| x.as_array()) else {
        return (0.0, None);
    };
    for ap in arr {
        if ap["position"]["coin"].as_str() != Some(coin) {
            continue;
        }
        if let Some(p) = ap.get("position") {
            let szi = parse_json_number(&p["szi"]).unwrap_or(0.0);
            let ep = p.get("entryPx").and_then(parse_json_number);
            return (szi, ep);
        }
        break;
    }
    (0.0, None)
}

/// clearinghouseState → adott coin perp `szi` (nincs pozíció → 0).
pub fn clearinghouse_coin_szi(state: &Value, coin: &str) -> f64 {
    clearinghouse_position_for_coin(state, coin).0
}

pub fn filter_cancel_oids_excluding_position_tpsl_triggers(
    frontend_orders: &Value,
    coin: &str,
    oids: Vec<u64>,
) -> Vec<u64> {
    let Some(arr) = frontend_orders.as_array() else { return oids; };
    let mut protected = HashSet::new();
    for ord in arr {
        if ord["coin"].as_str() != Some(coin) { continue; }
        // A `reduceOnly` close limit a kilépési struktúra része; azt ne canceljük
        // (különben könnyen megjelenik “plusz” close limit a TP/SL mellett).
        if hl_order_is_protected(ord) || json_truthy(&ord["reduceOnly"]) {
            if let Some(oid) = parse_order_oid(ord) { protected.insert(oid); }
        }
    }
    oids.into_iter().filter(|o| !protected.contains(o)).collect()
}

pub fn collect_ladder_cancel_oids_from_frontend(frontend_orders: &Value, coin: &str) -> Vec<u64> {
    let Some(arr) = frontend_orders.as_array() else { return Vec::new(); };
    let mut out = Vec::new();
    for ord in arr {
        if ord["coin"].as_str() != Some(coin) { continue; }
        // Ne canceljük a kilépési (protected) vagy `reduceOnly` close limitet,
        // különben a következő létra-körben újra legenerálódhat.
        if hl_order_is_protected(ord) || json_truthy(&ord["reduceOnly"]) { continue; }
        if let Some(oid) = parse_order_oid(ord) { out.push(oid); }
    }
    out
}



pub fn parse_spot_usdc_total_usd(spot: &Value) -> f64 {
    let Some(balances) = spot.get("balances").and_then(|b| b.as_array()) else { return 0.0; };
    let mut sum = 0.0;
    for b in balances {
        if b.get("coin").and_then(|c| c.as_str()) != Some("USDC") { continue; }
        if let Some(t) = b.get("total").and_then(parse_json_number) { sum += t; }
    }
    sum
}

fn margin_block_equity_usd(ms: &Value) -> Option<f64> {
    let av = ms.get("accountValue").and_then(parse_json_number);
    let tr = ms.get("totalRawUsd").and_then(parse_json_number);
    match (av, tr) {
        (Some(a), _) if a > 0.0 => Some(a),
        (_, Some(t)) if t > 0.0 => Some(t),
        (Some(a), Some(t)) => Some(a.max(t)),
        (Some(a), None) => Some(a),
        (None, Some(t)) => Some(t),
        _ => None,
    }
}

pub fn parse_clearinghouse_account_value_usd(state: &Value) -> Option<f64> {
    let mut best: Option<f64> = None;
    for key in ["marginSummary", "crossMarginSummary"] {
        if let Some(ms) = state.get(key) {
            if let Some(x) = margin_block_equity_usd(ms) {
                best = Some(best.map_or(x, |b| b.max(x)));
            }
        }
    }
    if let Some(w) = state.get("withdrawable").and_then(parse_json_number) {
        if w > 0.0 { best = Some(best.map_or(w, |b| b.max(w))); }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_prefers_positive_account_value() {
        let v = json!({"marginSummary": {"accountValue": "100.5", "totalRawUsd": "50"}});
        assert!((parse_clearinghouse_account_value_usd(&v).unwrap() - 100.5).abs() < 1e-6);
    }

    #[test]
    fn parse_falls_back_to_total_raw_when_account_zero() {
        let v = json!({"marginSummary": {"accountValue": "0", "totalRawUsd": "87.3"}});
        assert!((parse_clearinghouse_account_value_usd(&v).unwrap() - 87.3).abs() < 1e-6);
    }

    #[test]
    fn parse_falls_back_to_withdrawable() {
        let v = json!({
            "marginSummary": {"accountValue": "0", "totalRawUsd": "0"},
            "withdrawable": "42"
        });
        assert!((parse_clearinghouse_account_value_usd(&v).unwrap() - 42.0).abs() < 1e-6);
    }

    #[test]
    fn parse_all_zero() {
        let v = json!({
            "marginSummary": {"accountValue": "0", "totalRawUsd": "0"},
            "crossMarginSummary": {"accountValue": "0", "totalRawUsd": "0"},
            "withdrawable": "0"
        });
        assert_eq!(parse_clearinghouse_account_value_usd(&v), Some(0.0));
    }

    #[test]
    fn parse_spot_usdc_sums() {
        let s = json!({
            "balances": [
                {"coin": "USDC", "total": "50.5", "hold": "0"},
                {"coin": "PURR", "total": "1", "hold": "0"}
            ]
        });
        assert!((parse_spot_usdc_total_usd(&s) - 50.5).abs() < 1e-9);
    }

    #[test]
    fn clearinghouse_szi_parses_string_or_number() {
        let st = json!({
            "assetPositions": [{
                "position": {"coin": "SOL", "szi": "-0.32", "entryPx": "80.1"}
            }]
        });
        assert!((clearinghouse_coin_szi(&st, "SOL") + 0.32).abs() < 1e-9);
        let st2 = json!({
            "assetPositions": [{
                "position": {"coin": "SOL", "szi": 0.15, "entryPx": 79.5}
            }]
        });
        assert!((clearinghouse_coin_szi(&st2, "SOL") - 0.15).abs() < 1e-9);
        assert!(clearinghouse_has_error(&json!({"error": "foo"})));
        assert!(!clearinghouse_has_error(&json!({"error": null})));
        assert!(!clearinghouse_has_error(&st));
    }

    #[test]
    fn hl_order_is_protected_plain_limit_vs_triggers() {
        let ghost = json!({"coin": "SOL", "orderType": "Limit", "reduceOnly": true, "limitPx": "82.35"});
        assert!(!hl_order_is_protected(&ghost));
        assert!(hl_order_blocks_entry_ladder(&ghost));
        let tp = json!({"coin": "SOL", "orderType": "Take Profit Market"});
        assert!(hl_order_is_protected(&tp));
        let sl = json!({"coin": "SOL", "orderType": "Stop Market"});
        assert!(hl_order_is_protected(&sl));
        let trig_px = json!({"coin": "SOL", "orderType": "Limit", "triggerPx": "81.5"});
        assert!(hl_order_is_protected(&trig_px));
        let entry = json!({"coin": "SOL", "orderType": "Limit", "reduceOnly": false});
        assert!(!hl_order_blocks_entry_ladder(&entry));
    }

    #[test]
    fn collect_ladder_cancel_oids_skips_reduce_only_close() {
        let fe = json!([
            // reduceOnly close limit (nem védett "protected"-ként, de mi se canceljük)
            {"coin": "SOL", "oid": 1, "orderType": "Limit", "reduceOnly": true, "limitPx": "82.35"},
            // TP/SL trigger (protected) - szintén nem cancel
            {"coin": "SOL", "oid": 2, "orderType": "Take Profit Market"},
            // Maker létra (nem protected, nem reduceOnly)
            {"coin": "SOL", "oid": 3, "orderType": "Limit", "reduceOnly": false, "limitPx": "81.50"}
        ]);

        let oids = collect_ladder_cancel_oids_from_frontend(&fe, "SOL");
        assert_eq!(oids, vec![3]);
    }

    #[test]
    fn filter_cancel_oids_excluding_position_tpsl_triggers_keeps_reduce_only() {
        let fe = json!([
            {"coin": "SOL", "oid": 1, "orderType": "Limit", "reduceOnly": true, "limitPx": "82.35"},
            {"coin": "SOL", "oid": 2, "orderType": "Take Profit Market"},
            {"coin": "SOL", "oid": 3, "orderType": "Limit", "reduceOnly": false, "limitPx": "81.50"}
        ]);

        // A bemenetben mindhárom oid benne van: csak a 3-as maradhat.
        let kept = filter_cancel_oids_excluding_position_tpsl_triggers(&fe, "SOL", vec![1, 2, 3]);
        assert_eq!(kept, vec![3]);
    }
}
