use reqwest::Client;
use serde_json::{json, Value};
use std::collections::HashSet;
use ethers::core::types::Signature;

#[derive(Clone)]
pub struct HyperliquidClient {
    pub rest_client: Client,
    pub info_url: String,
    pub exchange_url: String,
}

impl HyperliquidClient {
    pub fn new(is_mainnet: bool) -> Self {
        // Pre-wärmte Verbindung für low latency
        let client = Client::builder()
            .pool_idle_timeout(None)
            .tcp_nodelay(true) // Disable Nagle's algorithm for min ping
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
        }
    }

    pub async fn get_meta(&self) -> Result<Value, reqwest::Error> {
        let payload = json!({"type": "meta"});
        let resp = self.rest_client.post(&self.info_url).json(&payload).send().await?;
        resp.json::<Value>().await
    }

    pub async fn get_user_state(&self, user: &str) -> Result<Value, reqwest::Error> {
        let payload = json!({
            "type": "clearinghouseState",
            "user": user
        });
        let resp = self.rest_client.post(&self.info_url).json(&payload).send().await?;
        resp.json::<Value>().await
    }

    /// Hyperliquid perp számlaérték (USD) a `clearinghouseState` válaszból.
    pub async fn get_account_value_usd(&self, user: &str) -> Result<f64, String> {
        let state = self
            .get_user_state(user)
            .await
            .map_err(|e| format!("clearinghouseState HTTP: {}", e))?;
        parse_clearinghouse_account_value_usd(&state).ok_or_else(|| {
            format!(
                "accountValue nem található (marginSummary/crossMarginSummary), válasz-részlet: {:?}",
                state.get("marginSummary").or_else(|| state.get("crossMarginSummary"))
            )
        })
    }

    pub async fn get_open_orders(&self, user: &str) -> Result<Value, reqwest::Error> {
        let payload = json!({
            "type": "openOrders",
            "user": user
        });
        let resp = self.rest_client.post(&self.info_url).json(&payload).send().await?;
        resp.json::<Value>().await
    }

    /// Nyitott orderek + `isPositionTpsl` / `isTrigger` / `orderType` — TP/SL szűréshez a törlés előtt.
    pub async fn get_frontend_open_orders(&self, user: &str) -> Result<Value, reqwest::Error> {
        let payload = json!({
            "type": "frontendOpenOrders",
            "user": user
        });
        let resp = self.rest_client.post(&self.info_url).json(&payload).send().await?;
        resp.json::<Value>().await
    }

    /// Küldi az EIP-712-vel aláírt actiont az exchange végpontra
    pub async fn send_l1_action<T: serde::Serialize>(
        &self,
        action: &T,
        nonce: u64,
        signature: Signature,
    ) -> Result<Value, reqwest::Error> {
        
        // Ethers Signature (U256) konvertálás HL formátumba (Hex string)
        let mut r_bytes = [0u8; 32];
        signature.r.to_big_endian(&mut r_bytes);
        let r_hex = hex::encode(r_bytes);
        
        let mut s_bytes = [0u8; 32];
        signature.s.to_big_endian(&mut s_bytes);
        let s_hex = hex::encode(s_bytes);
        // HL expects legacy Ethereum v (27 or 28) for EIP-712 L1 actions
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

        let resp = self.rest_client
            .post(&self.exchange_url)
            .json(&payload)
            .send()
            .await?;
            
        resp.json::<Value>().await
    }
}

fn parse_order_oid(ord: &Value) -> Option<u64> {
    ord["oid"]
        .as_u64()
        .or_else(|| ord["oid"].as_str().and_then(|v| v.parse().ok()))
}

/// Nem töröljük a position TP/SL és trigger típusú orderek OID-jét (clean slate előtt).
pub fn filter_cancel_oids_excluding_position_tpsl_triggers(
    frontend_orders: &Value,
    coin: &str,
    oids: Vec<u64>,
) -> Vec<u64> {
    let Some(arr) = frontend_orders.as_array() else {
        return oids;
    };
    let mut protected = HashSet::new();
    for ord in arr {
        if ord["coin"].as_str() != Some(coin) {
            continue;
        }
        let protect = ord["isPositionTpsl"].as_bool() == Some(true)
            || ord["isTrigger"].as_bool() == Some(true);
        if protect {
            if let Some(oid) = parse_order_oid(ord) {
                protected.insert(oid);
            }
        }
    }
    oids.into_iter()
        .filter(|o| !protected.contains(o))
        .collect()
}

/// Létra / sima limit OID-ek `frontendOpenOrders`-ból (TP/SL trigger nélkül).
pub fn collect_ladder_cancel_oids_from_frontend(frontend_orders: &Value, coin: &str) -> Vec<u64> {
    let Some(arr) = frontend_orders.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ord in arr {
        if ord["coin"].as_str() != Some(coin) {
            continue;
        }
        if ord["isPositionTpsl"].as_bool() == Some(true) {
            continue;
        }
        if ord["isTrigger"].as_bool() == Some(true) {
            continue;
        }
        if let Some(oid) = parse_order_oid(ord) {
            out.push(oid);
        }
    }
    out
}

fn parse_json_number(v: &Value) -> Option<f64> {
    if let Some(s) = v.as_str() {
        s.parse().ok()
    } else {
        v.as_f64()
    }
}

/// `marginSummary` vagy `crossMarginSummary` → `accountValue` (string vagy szám).
pub fn parse_clearinghouse_account_value_usd(state: &Value) -> Option<f64> {
    let from = |key: &str| {
        state
            .get(key)
            .and_then(|ms| ms.get("accountValue"))
            .and_then(parse_json_number)
    };
    from("marginSummary").or_else(|| from("crossMarginSummary"))
}
