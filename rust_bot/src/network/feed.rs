use futures_util::{SinkExt, StreamExt};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast, Mutex};
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
    /// Volumen-súlyozott VWAP az aktuális sessionre. 0.0 = még nincs elég adat.
    pub volume_vwap: f64,
    /// Aktuális session teljes forgalma (coin egységben).
    pub vwap_volume: f64,
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
            volume_vwap: 0.0,
            vwap_volume: 0.0,
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
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum SubscriptionData {
    #[serde(rename = "l2Book")]
    L2Book { coin: String },
    #[serde(rename = "userEvents")]
    UserEvents { user: String },
    #[serde(rename = "trades")]
    Trades { coin: String },
}

#[derive(Deserialize, Debug)]
struct WsResponse {
    channel: String,
    data: Option<serde_json::Value>,
}

/// Belső VWAP akkumulátor — egyetlen Mutex mögé zárva, minimális lockolással.
struct VwapAcc {
    sum_pv: f64,           // Σ(ár × forgalom)
    sum_v: f64,            // Σ(forgalom)
    session_start_ms: u64, // Session kezdete (trade timestamp ms)
    session_ms: u64,       // Session hossza ms-ban
    log_counter: u32,      // Periodikus log számláló
}

impl VwapAcc {
    fn new(session_hours: u32) -> Self {
        Self {
            sum_pv: 0.0,
            sum_v: 0.0,
            session_start_ms: 0,
            session_ms: session_hours as u64 * 3_600_000,
            log_counter: 0,
        }
    }

    /// Trade feldolgozása. Visszaadja az aktuális VWAP-ot és az összes forgalmat.
    fn process(&mut self, px: f64, sz: f64, trade_ts_ms: u64) -> (f64, f64) {
        // Session reset: az első trade-nél inicializál, vagy ha lejárt a session
        if self.session_start_ms == 0 {
            self.session_start_ms = trade_ts_ms;
        } else if trade_ts_ms >= self.session_start_ms + self.session_ms {
            let old_vwap = if self.sum_v > 0.0 { self.sum_pv / self.sum_v } else { 0.0 };
            info!(
                "🔄 VWAP session reset: lejárt {:?}h — előző VWAP={:.4} forgalom={:.2}",
                self.session_ms / 3_600_000,
                old_vwap,
                self.sum_v
            );
            self.sum_pv = 0.0;
            self.sum_v = 0.0;
            self.session_start_ms = trade_ts_ms;
        }

        self.sum_pv += px * sz;
        self.sum_v  += sz;

        let vwap = if self.sum_v > 0.0 { self.sum_pv / self.sum_v } else { 0.0 };

        // Periódikus log (~500 trade-enként)
        self.log_counter += 1;
        if self.log_counter >= 500 {
            self.log_counter = 0;
            info!(
                "📊 Volume VWAP: {:.4}  forgalom: {:.2}  (session: {:.1}h eltelt)",
                vwap,
                self.sum_v,
                (trade_ts_ms.saturating_sub(self.session_start_ms)) as f64 / 3_600_000.0
            );
        }

        (vwap, self.sum_v)
    }
}

pub struct HyperliquidFeed {
    pub coin: String,
    pub user_address: String,
    pub ws_url: String,
    pub state: Arc<RwLock<L2BookState>>,
    pub open_order_oids: Arc<Mutex<Vec<u64>>>,
    pub post_only_reject_flag: Arc<Mutex<bool>>,
    pub fill_tx: broadcast::Sender<FillEvent>,
    vwap_acc: Arc<Mutex<VwapAcc>>,
}

impl HyperliquidFeed {
    pub fn new(coin: &str, user_address: &str, is_mainnet: bool, vwap_session_hours: u32) -> Self {
        let (fill_tx, _) = broadcast::channel(100);
        let ws_url = if is_mainnet { HL_MAINNET_WSS_URL } else { HL_TESTNET_WSS_URL };
        Self {
            coin: coin.to_string(),
            user_address: user_address.to_string(),
            ws_url: ws_url.to_string(),
            state: Arc::new(RwLock::new(L2BookState {
                coin: coin.to_string(),
                ..Default::default()
            })),
            open_order_oids: Arc::new(Mutex::new(Vec::new())),
            post_only_reject_flag: Arc::new(Mutex::new(false)),
            fill_tx,
            vwap_acc: Arc::new(Mutex::new(VwapAcc::new(vwap_session_hours))),
        }
    }

    pub async fn start(self: Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            let mut reconnect_delay_secs: u64 = 1;
            loop {
                info!("🔗 Kapcsolódás a Hyperliquid WS-hez...");
                let url = Url::parse(&this.ws_url).unwrap();

                match connect_async(url).await {
                    Ok((ws_stream, _)) => {
                        info!("✅ Hyperliquid WS Connected");
                        reconnect_delay_secs = 1; // sikeres csatlakozás → reset

                        let (mut sink, mut stream) = ws_stream.split();

                        // L2 book feliratkozás
                        let sub_l2 = WsRequest::Subscribe {
                            subscription: SubscriptionData::L2Book { coin: this.coin.clone() },
                        };
                        sink.send(Message::Text(serde_json::to_string(&sub_l2).unwrap())).await.ok();

                        // Saját fill-ek
                        let sub_user = WsRequest::Subscribe {
                            subscription: SubscriptionData::UserEvents { user: this.user_address.clone() },
                        };
                        sink.send(Message::Text(serde_json::to_string(&sub_user).unwrap())).await.ok();

                        // Piaci ügyletek — Volume VWAP számításhoz
                        let sub_trades = WsRequest::Subscribe {
                            subscription: SubscriptionData::Trades { coin: this.coin.clone() },
                        };
                        sink.send(Message::Text(serde_json::to_string(&sub_trades).unwrap())).await.ok();
                        info!("📈 Trades stream feliratkozás: {}", this.coin);

                        // 30 másodperces ping — szerver-oldali timeout ellen
                        let mut ping_timer = tokio::time::interval(
                            tokio::time::Duration::from_secs(30)
                        );
                        ping_timer.tick().await;

                        let disconnect_reason = loop {
                            tokio::select! {
                                msg = stream.next() => {
                                    match msg {
                                        Some(Ok(Message::Text(text))) => {
                                            if text.contains("\"ping\"") {
                                                let _ = sink.send(Message::Text(r#"{"method":"pong"}"#.to_string())).await;
                                            } else {
                                                this.process_message(&text).await;
                                            }
                                        }
                                        Some(Ok(Message::Ping(data))) => {
                                            let _ = sink.send(Message::Pong(data)).await;
                                        }
                                        Some(Ok(Message::Close(frame))) => {
                                            break format!("szerver Close: {:?}", frame.map(|f| f.reason.to_string()));
                                        }
                                        Some(Err(e)) => {
                                            break format!("WS hiba: {}", e);
                                        }
                                        None => {
                                            break "stream lezárult (None)".to_string();
                                        }
                                        _ => {}
                                    }
                                }
                                _ = ping_timer.tick() => {
                                    if sink.send(Message::Text(r#"{"method":"ping"}"#.to_string())).await.is_err() {
                                        break "ping küldési hiba".to_string();
                                    }
                                }
                            }
                        };

                        warn!("⚠️ WS megszakadt ({}). Újracsatlakozás {}mp múlva...", disconnect_reason, reconnect_delay_secs);
                    }
                    Err(e) => {
                        warn!("❌ WS kapcsolódás sikertelen: {}. Újracsatlakozás {}mp múlva...", e, reconnect_delay_secs);
                    }
                }

                tokio::time::sleep(tokio::time::Duration::from_secs(reconnect_delay_secs)).await;
                // Exponenciális backoff: 1 → 2 → 4 → 8 → 16 → max 60s
                reconnect_delay_secs = (reconnect_delay_secs * 2).min(60);
            }
        });
    }

    async fn process_message(&self, text: &str) {
        if let Ok(response) = serde_json::from_str::<WsResponse>(text) {
            match response.channel.as_str() {
                "l2Book" => {
                    if let Some(data) = response.data { self.process_l2_book(data).await; }
                }
                "trades" => {
                    if let Some(data) = response.data { self.process_trades(data).await; }
                }
                "userEvents" | "user" => {
                    if let Some(data) = response.data { self.process_user_event(data).await; }
                }
                "post" => {
                    if let Some(data) = response.data {
                        self.track_post_order_oids(&data).await;
                        self.log_post_summary(&data).await;
                    }
                }
                "info" | "error" => {
                    tracing::debug!("📡 WS {} üzenet", response.channel);
                }
                _ => {}
            }
            return;
        }

        if let Ok(raw) = serde_json::from_str::<serde_json::Value>(text) {
            if raw.get("error").is_some() {
                error!("❌ WS POST ERROR: {}", text);
                return;
            }
            if raw.get("id").is_some() || raw.get("status").is_some() || raw.get("response").is_some() {
                info!("📡 WS POST FEEDBACK érkezett");
            }
        }
    }

    /// Tényleges piaci ügyletek feldolgozása → Volume VWAP frissítés.
    ///
    /// A Hyperliquid trades üzenet formátuma:
    /// `[{"coin":"SOL","side":"B","px":"152.50","sz":"1.5","time":1700000000000,"hash":"0x..."}]`
    async fn process_trades(&self, data: serde_json::Value) {
        let trades = match data.as_array() {
            Some(a) => a,
            None => return,
        };

        let mut batch_pv = 0.0f64;
        let mut batch_v  = 0.0f64;
        let mut latest_ts: u64 = 0;

        for trade in trades {
            if trade["coin"].as_str() != Some(self.coin.as_str()) { continue; }
            let px: f64 = trade["px"].as_str().unwrap_or("0").parse().unwrap_or(0.0);
            let sz: f64 = trade["sz"].as_str().unwrap_or("0").parse().unwrap_or(0.0);
            let ts: u64 = trade["time"].as_u64().unwrap_or(0);
            if px > 0.0 && sz > 0.0 {
                batch_pv += px * sz;
                batch_v  += sz;
                if ts > latest_ts { latest_ts = ts; }
            }
        }

        if batch_v <= 0.0 || latest_ts == 0 { return; }

        // Batch-enként egyszer lockolunk (nem trade-enként)
        let (vwap, total_v) = {
            let mut acc = self.vwap_acc.lock().await;
            // Minden trade-et egyenként kellene feldolgozni a timestamp alapú resethez,
            // de a batch-en belül a legkésőbbi timestamp-et használjuk közelítésként.
            acc.sum_pv += batch_pv;
            acc.sum_v  += batch_v;

            // Session reset detektálás (első inicializálás vagy lejárt session)
            if acc.session_start_ms == 0 {
                acc.session_start_ms = latest_ts;
            } else if latest_ts >= acc.session_start_ms + acc.session_ms {
                let old_vwap = if acc.sum_v > 0.0 { acc.sum_pv / acc.sum_v } else { 0.0 };
                info!(
                    "🔄 VWAP session reset ({}h) — előző VWAP={:.4}  forgalom={:.2}",
                    acc.session_ms / 3_600_000, old_vwap, acc.sum_v
                );
                acc.sum_pv = batch_pv;
                acc.sum_v  = batch_v;
                acc.session_start_ms = latest_ts;
            }

            let vwap = acc.sum_pv / acc.sum_v;

            acc.log_counter += 1;
            if acc.log_counter >= 500 {
                acc.log_counter = 0;
                let elapsed_h = (latest_ts.saturating_sub(acc.session_start_ms)) as f64 / 3_600_000.0;
                info!(
                    "📊 Volume VWAP: {:.4}  forgalom: {:.2}  (session: {:.1}h)",
                    vwap, acc.sum_v, elapsed_h
                );
            }

            (vwap, acc.sum_v)
        };

        // VWAP írása a megosztott állapotba
        let mut state = self.state.write().await;
        state.volume_vwap = vwap;
        state.vwap_volume = total_v;
    }

    async fn log_post_summary(&self, data: &serde_json::Value) {
        let id = data["id"].as_u64().unwrap_or(0);
        let payload = &data["response"]["payload"];
        let status = payload["status"].as_str().unwrap_or("unknown");
        let resp_type = payload["response"]["type"].as_str().unwrap_or("");

        if status != "ok" {
            warn!("❌ WS POST id={} status={} — válasz: {}", id, status,
                serde_json::to_string(data).unwrap_or_default());
            return;
        }

        if resp_type == "order" {
            let statuses = payload["response"]["data"]["statuses"].as_array();
            let count = statuses.map(|a| a.len()).unwrap_or(0);
            let mut resting = 0usize;
            let mut filled = 0usize;
            let mut errored = 0usize;
            let mut other = 0usize;
            let mut sample_error: Option<String> = None;

            if let Some(arr) = statuses {
                for s in arr {
                    if s.get("resting").is_some() { resting += 1; }
                    else if s.get("filled").is_some() { filled += 1; }
                    else if s.get("error").is_some() {
                        errored += 1;
                        if sample_error.is_none() {
                            sample_error = s.get("error").and_then(|v| v.as_str()).map(|v| v.to_string());
                        }
                    } else { other += 1; }
                }
            }

            if let Some(ref err) = sample_error {
                if err.contains("Post only order would have immediately matched") {
                    *self.post_only_reject_flag.lock().await = true;
                }
                warn!("⚠️ POST order (id={}): resting={} filled={} errored={} other={} err={}",
                    id, resting, filled, errored, other, err);
            } else {
                *self.post_only_reject_flag.lock().await = false;
                info!("✅ POST order ok (id={} statuses={} resting={} filled={} errored={} other={})",
                    id, count, resting, filled, errored, other);
            }
            return;
        }

        if resp_type == "cancel" {
            let statuses = payload["response"]["data"]["statuses"].as_array();
            let (mut success, mut non_success) = (0usize, 0usize);
            if let Some(arr) = statuses {
                for s in arr {
                    if s.as_str() == Some("success") { success += 1; } else { non_success += 1; }
                }
            }
            info!("🧹 POST cancel ok (id={} success={} other={})", id, success, non_success);
            return;
        }

        info!("📡 POST ok (id={} type={})", id, resp_type);
    }

    async fn track_post_order_oids(&self, data: &serde_json::Value) {
        let response = &data["response"];
        if response["type"].as_str().unwrap_or("") != "action" { return; }
        let action_payload = &response["payload"];
        if action_payload["status"].as_str().unwrap_or("") != "ok" { return; }
        if action_payload["response"]["type"].as_str().unwrap_or("") != "order" { return; }

        if let Some(statuses) = action_payload["response"]["data"]["statuses"].as_array() {
            let mut tracked = self.open_order_oids.lock().await;
            let before = tracked.len();
            for status in statuses {
                let oid = status["resting"]["oid"].as_u64()
                    .or_else(|| status["resting"]["oid"].as_str().and_then(|v| v.parse().ok()))
                    .or_else(|| status["oid"].as_u64())
                    .or_else(|| status["oid"].as_str().and_then(|v| v.parse().ok()));
                if let Some(oid) = oid { tracked.push(oid); }
            }
            let added = tracked.len().saturating_sub(before);
            if added > 0 {
                info!("🧷 OID TRACK: {} új OID mentve", added);
            } else {
                warn!("⚠️ OID TRACK: order POST ok, de nincs OID a statuses-ban");
            }
        }
    }

    async fn process_l2_book(&self, data: serde_json::Value) {
        if let (Some(levels), Some(time), Some(coin)) = (
            data["levels"].as_array(),
            data["time"].as_u64(),
            data["coin"].as_str(),
        ) {
            if levels.len() == 2 && coin == self.coin {
                let bids = &levels[0];
                let asks = &levels[1];

                if let (Some(best_bid), Some(best_ask)) = (bids.get(0), asks.get(0)) {
                    let bid_px: f64 = best_bid["px"].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                    let ask_px: f64 = best_ask["px"].as_str().unwrap_or("0").parse().unwrap_or(0.0);

                    let mut bid_vol = 0.0f64;
                    let mut ask_vol = 0.0f64;
                    let bid_depth = bids.as_array().map(|a| a.len()).unwrap_or(0).min(3);
                    let ask_depth = asks.as_array().map(|a| a.len()).unwrap_or(0).min(3);
                    for i in 0..bid_depth {
                        bid_vol += bids[i]["sz"].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0);
                    }
                    for i in 0..ask_depth {
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
        if let Some(fills) = data["fills"].as_array() {
            for fill in fills {
                let event = FillEvent {
                    coin: fill["coin"].as_str().unwrap_or("").to_string(),
                    px: fill["px"].as_str().unwrap_or("0").parse().unwrap_or(0.0),
                    sz: fill["sz"].as_str().unwrap_or("0").parse().unwrap_or(0.0),
                    side: fill["side"].as_str().unwrap_or("").to_string(),
                    fee: fill["fee"].as_str().unwrap_or("0").parse().unwrap_or(0.0),
                };
                let _ = self.fill_tx.send(event);
            }
        }
    }
}
