use futures_util::{SinkExt, StreamExt};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tracing::{error, info, warn};

// Hyperliquid WebSocket URL
const HL_WSS_URL: &str = "wss://api.hyperliquid.xyz/ws";

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

// Subscription Payload
#[derive(Serialize)]
struct SubscriptionMsg {
    method: String,
    subscription: SubscriptionData,
}

#[derive(Serialize)]
struct SubscriptionData {
    #[serde(rename = "type")]
    sub_type: String,
    coin: String,
}

// Hyperliquid L2 Book Response Parsing
#[derive(Deserialize, Debug)]
struct WsResponse {
    channel: String,
    data: Option<L2BookData>,
}

#[derive(Deserialize, Debug)]
struct L2BookData {
    coin: String,
    levels: Vec<Vec<LevelData>>, // [ [bids], [asks] ]
    time: u64,
}

#[derive(Deserialize, Debug)]
struct LevelData {
    px: String,
    sz: String,
    n: u32,
}

pub struct HyperliquidFeed {
    pub coin: String,
    pub state: Arc<RwLock<L2BookState>>,
}

impl HyperliquidFeed {
    pub fn new(coin: &str) -> Self {
        Self {
            coin: coin.to_string(),
            state: Arc::new(RwLock::new(L2BookState {
                coin: coin.to_string(),
                ..Default::default()
            })),
        }
    }

    pub async fn start(&self) {
        let coin_clone = self.coin.clone();
        let state_clone = self.state.clone();

        tokio::spawn(async move {
            loop {
                info!("🔗 Kapcsolódás a Hyperliquid WS-hez (Piac: {})...", coin_clone);
                let url = Url::parse(HL_WSS_URL).unwrap();
                
                match connect_async(url).await {
                    Ok((mut ws_stream, _)) => {
                        info!("✅ Hyperliquid WS Connected (L2 Book stream)");
                        
                        // Subscribe message
                        let sub = SubscriptionMsg {
                            method: "subscribe".to_string(),
                            subscription: SubscriptionData {
                                sub_type: "l2Book".to_string(),
                                coin: coin_clone.clone(),
                            },
                        };
                        
                        let sub_json = serde_json::to_string(&sub).unwrap();
                        if let Err(e) = ws_stream.send(Message::Text(sub_json)).await {
                            error!("❌ Hiba a feliratkozáskor: {}", e);
                            continue;
                        }

                        // Listen loop
                        while let Some(msg) = ws_stream.next().await {
                            match msg {
                                Ok(Message::Text(text)) => {
                                    Self::process_message(&text, &state_clone).await;
                                }
                                Ok(Message::Ping(_)) => {
                                    // ping-pong is handled automatically by tokio-tungstenite, but we can log
                                }
                                Err(e) => {
                                    error!("🔌 WS Hiba olvasás közben: {}", e);
                                    break;
                                }
                                _ => {}
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

    async fn process_message(text: &str, state: &Arc<RwLock<L2BookState>>) {
        if let Ok(response) = serde_json::from_str::<WsResponse>(text) {
            if response.channel == "l2Book" {
                if let Some(data) = response.data {
                    if data.levels.len() == 2 {
                        let bids = &data.levels[0];
                        let asks = &data.levels[1];

                        if let (Some(best_bid), Some(best_ask)) = (bids.first(), asks.first()) {
                            let bid_px: f64 = best_bid.px.parse().unwrap_or(0.0);
                            let ask_px: f64 = best_ask.px.parse().unwrap_or(0.0);
                            let mid: f64 = (bid_px + ask_px) / 2.0;

                            // Calculate Top3 Volume Imbalance
                            let mut bid_vol = 0.0;
                            let mut ask_vol = 0.0;
                            
                            for i in 0..std::cmp::min(3, bids.len()) {
                                bid_vol += bids[i].sz.parse::<f64>().unwrap_or(0.0);
                            }
                            for i in 0..std::cmp::min(3, asks.len()) {
                                ask_vol += asks[i].sz.parse::<f64>().unwrap_or(0.0);
                            }
                            
                            let total_vol = bid_vol + ask_vol;
                            let imbalance = if total_vol > 0.0 { bid_vol / total_vol } else { 0.5 };

                            // Update shared state behind lock
                            let mut locked_state = state.write().await;
                            locked_state.best_bid = bid_px;
                            locked_state.best_ask = ask_px;
                            locked_state.mid_price = mid;
                            locked_state.imbalance = imbalance;
                            locked_state.last_update_ts = data.time;
                        }
                    }
                }
            }
        }
    }
}
