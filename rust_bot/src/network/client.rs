use reqwest::Client;
use serde_json::Value;

pub struct HyperliquidClient {
    pub rest_client: Client,
    pub base_url: String,
}

impl HyperliquidClient {
    pub fn new() -> Self {
        // Pre-wärmte Verbindung für low latency
        let client = Client::builder()
            .pool_idle_timeout(None)
            .tcp_nodelay(true) // Disable Nagle's algorithm for min ping
            .build()
            .unwrap();

        Self {
            rest_client: client,
            base_url: "https://api.hyperliquid.xyz/info".to_string(), // TODO: add /exchange
        }
    }

    pub async fn get_meta(&self) -> Result<Value, reqwest::Error> {
        let payload = serde_json::json!({
            "type": "meta"
        });

        let resp = self.rest_client
            .post(&self.base_url)
            .json(&payload)
            .send()
            .await?;
            
        resp.json::<Value>().await
    }
}
