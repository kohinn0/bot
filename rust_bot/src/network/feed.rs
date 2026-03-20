use futures_util::{SinkExt, StreamExt};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc, broadcast, Mutex};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tracing::{error, info, warn};

const HL_MAINNET_WSS_URL: &str = "wss://api.hyperliquid.xyz/ws";
const HL_TESTNET_WSS_URL: &str = "wss://api.hyperliquid-testnet.xyz/ws";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L2BookState {
    pub coin: String,
    pub mid_price: f64,
    pub best_bid: f64,
    pub best_ask: f64,
    pub imbalance: f64,
    pub last_update_ts: u64,
}

impl Default for L2BookState {
    fn default() -> Self {
        Self {
            coin: String::new(),
            mid_price: 0.0,
            best_bid: 0.0,
            best_ask: 0.0,
            imbalance: 0.5,
            last_update_ts: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FillEvent {
    pub coin: String,
    pub px: f64,
    pub sz: f64,
    pub side: String,
    pub fee: f64,
}

#[derive(Serialize)]
#[serde(tag = "method")]
enum WsRequest {
    #[serde(rename = "subscribe")]
    Subscribe { subscription: SubscriptionData },
    #[serde(rename = "post")]
    Post {
        id: u64,
        request: WsPostRequest,
    },
}

#[derive(Serialize)]
struct WsPostRequest {
    #[serde(rename = "type")]
    request_type: String,
    payload: serde_json::Value,
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum SubscriptionData {
    #[serde(rename = "l2Book")]
    L2Book { coin: String },
    #[serde(rename = "userEvents")]
    UserEvents { user: String },
}

#[derive(Deserialize, Debug)]
struct WsResponse {
    channel: String,
    data: Option<serde_json::Value>,
}

pub struct HyperliquidFeed {
    pub coin: String,
    pub user_address: String,
    pub ws_url: String,
    pub state: Arc<RwLock<L2BookState>>,
    pub open_order_oids: Arc<Mutex<Vec<u64>>>,
    pub fill_tx: broadcast::Sender<FillEvent>,
    cmd_tx: mpsc::UnboundedSender<serde_json::Value>,
}

impl HyperliquidFeed {
    pub fn new(coin: &str, user_address: &str, is_mainnet: bool) -> (Self, mpsc::UnboundedReceiver<serde_json::Value>) {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (fill_tx, _) = broadcast::channel(100);
        let ws_url = if is_mainnet { HL_MAINNET_WSS_URL } else { HL_TESTNET_WSS_URL };
        
        let feed = Self {
            coin: coin.to_string(),
            user_address: user_address.to_string(),
            ws_url: ws_url.to_string(),
            state: Arc::new(RwLock::new(L2BookState {
                coin: coin.to_string(),
                ..Default::default()
            })),
            open_order_oids: Arc::new(Mutex::new(Vec::new())),
            fill_tx,
            cmd_tx,
        };
        (feed, cmd_rx)
    }

    pub fn send_action(&self, action: serde_json::Value) {
        let _ = self.cmd_tx.send(action);
    }

    pub async fn start(self: Arc<Self>, mut cmd_rx: mpsc::UnboundedReceiver<serde_json::Value>) {
        let this = self.clone();
        
        tokio::spawn(async move {
            loop {
                info!("🔗 Kapcsolódás a Hyperliquid WS-hez...");
                let url = Url::parse(&this.ws_url).unwrap();
                
                match connect_async(url).await {
                    Ok((mut ws_stream, _)) => {
                        info!("✅ Hyperliquid WS Connected (Market + User stream)");
                        
                        // 1. Subscribe to L2 Book
                        let sub_l2 = WsRequest::Subscribe {
                            subscription: SubscriptionData::L2Book { coin: this.coin.clone() },
                        };
                        ws_stream.send(Message::Text(serde_json::to_string(&sub_l2).unwrap())).await.ok();

                        // 2. Subscribe to User Events
                        let sub_user = WsRequest::Subscribe {
                            subscription: SubscriptionData::UserEvents { user: this.user_address.clone() },
                        };
                        ws_stream.send(Message::Text(serde_json::to_string(&sub_user).unwrap())).await.ok();

                        loop {
                            tokio::select! {
                                // Handle incoming WS messages
                                Some(msg) = ws_stream.next() => {
                                    match msg {
                                        Ok(Message::Text(text)) => {
                                            this.process_message(&text).await;
                                        }
                                        Ok(Message::Close(_)) => break,
                                        Err(_) => break,
                                        _ => {}
                                    }
                                }
                                // Handle outgoing actions (Orders) - This is the LOW LATENCY path
                                Some(payload) = cmd_rx.recv() => {
                                    let req_id = payload["nonce"].as_u64().unwrap_or(
                                        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
                                    );
                                    
                                    // HL WS post requires wrapper:
                                    // request: { type: "action", payload: { action, nonce, signature } }
                                    let req = WsRequest::Post {
                                        id: req_id,
                                        request: WsPostRequest {
                                            request_type: "action".to_string(),
                                            payload,
                                        },
                                    };
                                    if let Ok(json) = serde_json::to_string(&req) {
                                        if let Err(e) = ws_stream.send(Message::Text(json)).await {
                                            error!("❌ Hiba a WS megbízás küldésekor: {}", e);
                                        } else {
                                            info!("📤 Megbízás kiküldve (post method, ID: {})", req_id);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!("❌ Sikertelen WS kapcsolódás: {}", e);
                    }
                }

                warn!("⚠️ WS Kapcsolat megszakadt. Újracsatlakozás 1mp múlva...");
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
        });
    }

    async fn process_message(&self, text: &str) {
        if let Ok(response) = serde_json::from_str::<WsResponse>(text) {
            match response.channel.as_str() {
                "l2Book" => {
                    if let Some(data) = response.data {
                        self.process_l2_book(data).await;
                    }
                }
                "userEvents" | "user" => {
                    if let Some(data) = response.data {
                        self.process_user_event(data).await;
                    }
                }
                "post" => {
                    if let Some(data) = response.data {
                        self.track_post_order_oids(&data).await;
                        self.log_post_summary(&data).await;
                    }
                }
                "info" | "error" => {
                    info!("📡 WS {} üzenet érkezett", response.channel);
                }
                _ => {
                    // Ignore noisy channels we don't explicitly track.
                }
            }
            return;
        }

        // WS "post" responses often don't include "channel" and would be missed otherwise.
        if let Ok(raw) = serde_json::from_str::<serde_json::Value>(text) {
            let has_id = raw.get("id").is_some();
            let has_status = raw.get("status").is_some();
            let has_response = raw.get("response").is_some();
            let has_error = raw.get("error").is_some();

            if has_error {
                error!("❌ WS POST ERROR: {}", text);
                return;
            }

            if has_id || has_status || has_response {
                info!("📡 WS POST FEEDBACK érkezett");
                return;
            }
        }
    }

    async fn log_post_summary(&self, data: &serde_json::Value) {
        let id = data["id"].as_u64().unwrap_or(0);
        let response = &data["response"];
        let payload = &response["payload"];
        let status = payload["status"].as_str().unwrap_or("unknown");
        let resp_type = payload["response"]["type"].as_str().unwrap_or("");

        if status != "ok" {
            info!("📡 POST id={} status={}", id, status);
            return;
        }

        if resp_type == "order" {
            let count = payload["response"]["data"]["statuses"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0);
            info!("✅ POST order ok (id={}, statuses={})", id, count);
            return;
        }

        if resp_type == "cancel" {
            let statuses = payload["response"]["data"]["statuses"].as_array();
            let mut success = 0usize;
            let mut non_success = 0usize;
            if let Some(arr) = statuses {
                for s in arr {
                    if s.as_str() == Some("success") {
                        success += 1;
                    } else {
                        non_success += 1;
                    }
                }
            }
            info!("🧹 POST cancel ok (id={}, success={}, other={})", id, success, non_success);
            return;
        }

        info!("📡 POST ok (id={}, type={})", id, resp_type);
    }

    async fn track_post_order_oids(&self, data: &serde_json::Value) {
        let response = &data["response"];
        if response["type"].as_str().unwrap_or("") != "action" {
            return;
        }
        let action_payload = &response["payload"];
        if action_payload["status"].as_str().unwrap_or("") != "ok" {
            return;
        }
        if action_payload["response"]["type"].as_str().unwrap_or("") != "order" {
            return;
        }
        if let Some(statuses) = action_payload["response"]["data"]["statuses"].as_array() {
            let mut tracked = self.open_order_oids.lock().await;
            for status in statuses {
                if let Some(oid) = status["resting"]["oid"].as_u64() {
                    tracked.push(oid);
                }
            }
        }
    }

    async fn process_l2_book(&self, data: serde_json::Value) {
        // [Paroljuk a bonyolult L2 struktúrát...]
        if let (Some(levels), Some(time), Some(coin)) = (data["levels"].as_array(), data["time"].as_u64(), data["coin"].as_str()) {
            if levels.len() == 2 && coin == self.coin {
                let bids = &levels[0];
                let asks = &levels[1];

                if let (Some(best_bid), Some(best_ask)) = (bids.get(0), asks.get(0)) {
                    let bid_px: f64 = best_bid["px"].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                    let ask_px: f64 = best_ask["px"].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                    
                    let mut bid_vol = 0.0;
                    let mut ask_vol = 0.0;
                    for i in 0..std::cmp::min(3, bids.as_array().map(|a| a.len()).unwrap_or(0)) {
                        bid_vol += bids[i]["sz"].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0);
                    }
                    for i in 0..std::cmp::min(3, asks.as_array().map(|a| a.len()).unwrap_or(0)) {
                        ask_vol += asks[i]["sz"].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0);
                    }

                    let mut state = self.state.write().await;
                    state.best_bid = bid_px;
                    state.best_ask = ask_px;
                    state.mid_price = (bid_px + ask_px) / 2.0;
                    let total = bid_vol + ask_vol;
                    state.imbalance = if total > 0.0 { bid_vol / total } else { 0.5 };
                    state.last_update_ts = time;
                }
            }
        }
    }

    async fn process_user_event(&self, data: serde_json::Value) {
        // User events: fills, fundings, etc.
        if let Some(fills) = data["fills"].as_array() {
            for fill in fills {
                let event = FillEvent {
                    coin: fill["coin"].as_str().unwrap_or("").to_string(),
                    px: fill["px"].as_str().unwrap_or("0").parse().unwrap_or(0.0),
                    sz: fill["sz"].as_str().unwrap_or("0").parse().unwrap_or(0.0),
                    side: fill["side"].as_str().unwrap_or("").to_string(),
                    fee: fill["fee"].as_str().unwrap_or("0").parse().unwrap_or(0.0),
                };
                info!("💎 FILL ÉSZLELVE: {} {} @ {} (Fee: ${})", event.side, event.sz, event.px, event.fee);
                let _ = self.fill_tx.send(event);
            }
        }
    }
}
