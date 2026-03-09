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

    /// Küldi az EIP-712-vel aláírt actiont az exchange végpontra
    pub async fn send_l1_action(
        &self,
        action: Value,
        nonce: u64,
        signature: Signature,
    ) -> Result<Value, reqwest::Error> {
        
        // Ethers Signature konvertálás HL formátumba
        let r = hex::encode(signature.r.0);
        let s = hex::encode(signature.s.0);
        // A HL egy 27/28 - 27 offsettel nézni a v-t
        let v_val = signature.v as u8;
        let v = if v_val >= 27 { v_val - 27 } else { v_val };
        
        let payload = json!({
            "action": action,
            "nonce": nonce,
            "signature": {
                "r": format!("0x{}", r),
                "s": format!("0x{}", s),
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
