use reqwest::Client;
use serde_json::{json, Value};
use ethers::core::types::Signature;

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

    pub async fn get_open_orders(&self, user: &str) -> Result<Value, reqwest::Error> {
        let payload = json!({
            "type": "openOrders",
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
