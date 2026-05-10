//! SebessegBot: **szignál loop** (maker létra) és **reconcile** (pozíció, TP/SL, dust) párhuzamosan fut.
//! Közös `current_position` + Hyperliquid REST. Új létra csak akkor mehet, ha a clearinghouse szerint
//! nincs pozíció, létra-cancel után a könyv üres erre a coinra, és nincs blokkoló order (TP/SL / reduce-only).
//! Minden **L1** (aláírás + POST) a `HyperliquidL1Gate`-en megy: soros végrehajtás + szigorúan növekvő nonce.
//! A szignál-létra a gate alatt kétszer (120 ms különbséggel) lekéri a clearinghouse-t és a könyvet — a HL API
//! néha később mutatja a TP/SL-t; egy olvasás alatt átcsúszhatna a maker létra.
//! A szignál-intervallum lejárta után **azonnal** élő `get_user_state` fut (nem csak min_slice után), különben
//! elavult `pos_sim` mellett a pozícióellenőrzés ki sem futna.
//! A `clearinghouseState` **több újrapróbálással** jön (`get_user_state_retry_ok`): átmeneti `{ "error": ... }` / HTTP
//! hiba ne hagyja ki a `pos_sim` frissítését egy egész ticken (reconcile + szignál + L1 gate).
//! Részleges fill után: sikeres TP/SL batch után **azonnal** friss `frontendOpenOrders` + létra-maradék cancel (sweep),
//! különben a könyvön maradhatnak belépő limitek a védelem mellett.

mod config;
mod network;
mod logic;

/// Eszkaláló trade-tilalom: egymást követő veszteséges zárások után exponenciálisan nő a pihenő.
/// 1 veszteség → 10 perc, 2 → 30 perc, 3+ → 2 óra; nyereséges zárás nullázza a számlálót.
struct CooldownState {
    until: Option<std::time::Instant>,
    consecutive_losses: u32,
}

impl CooldownState {
    fn new() -> Self {
        Self { until: None, consecutive_losses: 0 }
    }

    fn on_loss(&mut self) {
        self.consecutive_losses += 1;
        let secs: u64 = match self.consecutive_losses {
            1 => 600,   // 10 perc
            2 => 1800,  // 30 perc
            _ => 7200,  // 2 óra (3+)
        };
        self.until = Some(std::time::Instant::now() + std::time::Duration::from_secs(secs));
        tracing::warn!(
            "🛑 TRADE COOLDOWN #{}: {} egymást követő veszteség — {} perc szünet",
            self.consecutive_losses, self.consecutive_losses, secs / 60
        );
    }

    fn on_profit(&mut self) {
        if self.consecutive_losses > 0 {
            tracing::info!(
                "✅ Nyereséges zárás — consecutive_losses {} → 0 reset",
                self.consecutive_losses
            );
        }
        self.consecutive_losses = 0;
        self.until = None;
    }

    fn is_active(&self) -> bool {
        self.until.map_or(false, |t| std::time::Instant::now() < t)
    }

    fn remaining_secs(&self) -> u64 {
        self.until
            .filter(|&t| t > std::time::Instant::now())
            .map(|t| t.duration_since(std::time::Instant::now()).as_secs())
            .unwrap_or(0)
    }
}

use dotenvy::dotenv;
use tracing::info;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::FmtSubscriber;
use std::sync::Arc;
use crate::config::AppConfig;
use crate::logic::l1_gate::HyperliquidL1Gate;
use crate::logic::signer::HyperliquidSigner;
use crate::logic::signal::SignalEngine;
use crate::logic::order_manager::OrderManager;
use crate::network::client::{
    CancelAck,
    OrderAck,
    clearinghouse_coin_szi,
    clearinghouse_position_for_coin,
    collect_ladder_cancel_oids_from_frontend,
    collect_resting_oids_from_exchange_response,
    exchange_action_ok_or_warn,
    exchange_cancel_ack_or_warn,
    exchange_order_ack_or_warn,
    filter_cancel_oids_excluding_position_tpsl_triggers,
    frontend_has_any_open_order_for_coin,
    frontend_has_blocking_orders_for_coin,
    get_frontend_open_orders_retry_ok,
    get_frontend_open_orders_retry_ok_fast,
    get_user_state_retry_ok,
    get_user_state_retry_ok_fast,
    hl_order_is_protected,
    HyperliquidClient,
};
use crate::network::feed::HyperliquidFeed;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let subscriber = FmtSubscriber::builder().with_env_filter(filter).finish();
    tracing::subscriber::set_global_default(subscriber).expect("Failed to set tracing subscriber");

    // Panic hook: bármely (spawn-olt) taszk panic-ja a tracing log-ba kerül, ne csak a stderr-be.
    // Így a "néma" crash-ek (panic = "unwind" + eldobott JoinHandle) is visszakereshetők a logból.
    std::panic::set_hook(Box::new(|info| {
        let loc = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_else(|| "<?>".to_string());
        let msg = info.payload().downcast_ref::<&'static str>().map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        tracing::error!("💥 PANIC @ {} — {}", loc, msg);
    }));

    dotenv().ok();
    info!("🚀 INICIALIZÁLÁS: SebessegBot V4.6 (Stricter Signal Conditions) 🚀");

    let app_config = AppConfig::load();
    let coin = app_config.strategy.coin.clone();
    let is_mainnet = app_config.is_mainnet;
    let signer = HyperliquidSigner::new(&app_config.private_key);
    let rest_client = HyperliquidClient::new(is_mainnet, Some(app_config.hl_perp_dex.clone()));
    let signer_addr = signer.get_address().to_string();
    let hl_user = app_config.hl_user_address.as_ref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| signer_addr.clone());

    let meta = rest_client.get_meta().await.expect("❌ Meta hiba");
    let (mut asset_idx, mut sz_decimals) = (0u32, 0u32);
    if let Some(universe) = meta["universe"].as_array() {
        for (idx, cd) in universe.iter().enumerate() {
            if cd["name"].as_str().unwrap_or("") == coin {
                asset_idx = idx as u32;
                sz_decimals = cd["szDecimals"].as_u64().unwrap_or(0) as u32;
                break;
            }
        }
    }

    let feed = Arc::new(HyperliquidFeed::new(&coin, &hl_user, is_mainnet, app_config.strategy.vwap_session_hours));
    let state_ref = feed.state.clone();
    feed.clone().start().await;

    let use_hl_equity = app_config.use_wallet_balance_for_sizing && !app_config.is_dry_run;
    let initial_equity = if use_hl_equity {
        rest_client.get_account_value_usd(hl_user.as_str()).await.unwrap_or(app_config.starting_equity_usd)
    } else { app_config.starting_equity_usd };

    let session_start_equity = initial_equity;
    let wallet_equity = Arc::new(tokio::sync::Mutex::new(initial_equity));
    let target_notional_usd = Arc::new(tokio::sync::Mutex::new(app_config.strategy.notional_per_level_usd(initial_equity)));
    let current_position = Arc::new(tokio::sync::Mutex::new(0.0f64));
    let last_volatility = Arc::new(tokio::sync::Mutex::new(0.01f64));
    // Trade cooldown: veszteséges zárás után eszkaláló tilalom (revenge trading elleni védelem)
    let trade_cooldown: Arc<tokio::sync::Mutex<CooldownState>> =
        Arc::new(tokio::sync::Mutex::new(CooldownState::new()));

    let mut signal_engine = SignalEngine::new(app_config.strategy.clone());
    let mut order_manager = OrderManager::new(app_config.strategy.clone(), asset_idx, sz_decimals);
    let (signer, rest_client) = (Arc::new(signer), Arc::new(rest_client));
    let l1_gate = Arc::new(HyperliquidL1Gate::new());

    // Háttérben: számlaérték frissítése 30mp-enként — drawdown limit + notional sizing
    // Nélküle wallet_equity soha nem változik, a 10% drawdown limit sosem triggerel.
    if use_hl_equity {
        let rest_w = rest_client.clone();
        let addr_w = hl_user.clone();
        let wallet_w = wallet_equity.clone();
        let target_w = target_notional_usd.clone();
        let strat_w = app_config.strategy.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                if let Ok(v) = rest_w.get_account_value_usd(&addr_w).await {
                    if v.is_finite() && v > 0.0 {
                        *wallet_w.lock().await = v;
                        *target_w.lock().await = strat_w.notional_per_level_usd(v);
                    }
                }
            }
        });
    }

    // Leverage beállítás indításkor (L1 gate: soros nonce + válasz ellenőrzés)
    if !app_config.is_dry_run {
        let lev = order_manager.build_leverage_payload();
        let res: Result<serde_json::Value, String> = l1_gate
            .run(|nonce| {
                let s = signer.clone();
                let r = rest_client.clone();
                async move {
                    let sig = s
                        .sign_l1_action(&lev, nonce, is_mainnet)
                        .await
                        .map_err(|e| format!("aláírás: {}", e))?;
                    r.send_l1_action(&lev, nonce, sig)
                        .await
                        .map_err(|e| format!("HTTP: {}", e))
                }
            })
            .await;
        match res {
            Ok(body) => {
                if exchange_action_ok_or_warn("leverage", &Ok(body)) {
                    info!("✅ Leverage {}x beállítva", app_config.strategy.leverage);
                }
            }
            Err(e) => tracing::warn!("⚠️ Leverage L1: {}", e),
        }
    }

    let (signer_t, feed_t, rest_client_t, pos_sim_t, vol_sim_t, state_t, hl_user_t, target_notional_t, wallet_equity_t, l1_gate_t) =
        (signer.clone(), feed.clone(), rest_client.clone(), current_position.clone(), last_volatility.clone(), state_ref.clone(), hl_user.clone(), target_notional_usd.clone(), wallet_equity.clone(), l1_gate.clone());
    let trade_cooldown_t = trade_cooldown.clone();

    let mut last_signal_time = std::time::Instant::now() - std::time::Duration::from_secs(60);
    let min_signal_interval = std::time::Duration::from_millis(app_config.strategy.min_signal_interval_ms);
    let coin_signal = coin.clone();
    let strategy_for_signal = app_config.strategy.clone();
    let is_mainnet_for_signal = app_config.is_mainnet;

    tokio::spawn(async move {
        loop {
            let (mid, imbalance, feed_ts, volume_vwap) = {
                let s = state_t.read().await;
                let m = if s.best_bid > 0.0 && s.best_ask > 0.0 {
                    (s.best_bid + s.best_ask) / 2.0
                } else {
                    0.0
                };
                (m, s.imbalance, s.last_update_ts, s.volume_vwap)
            };
            // Elavult feed ellenőrzés: ha a WS >2s óta nem küldött adatot, ne tüzeljünk.
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            if feed_ts > 0 && now_ms.saturating_sub(feed_ts) > 2000 {
                tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                continue;
            }
            if let Some(signal) = signal_engine.tick(mid, imbalance, volume_vwap) {
                // Csak akkor hívunk HL-t, ha a szignál-intervallum lejárt — különben 5 ms-onként spammelnénk.
                if last_signal_time.elapsed() < min_signal_interval {
                    continue;
                }

                // Trade cooldown: eszkaláló tilalom veszteséges zárások után
                {
                    let cd = trade_cooldown_t.lock().await;
                    if cd.is_active() {
                        tracing::debug!("Trade cooldown aktív — {}s van hátra", cd.remaining_secs());
                        continue;
                    }
                }

                // Élő clearinghouse **előbb**, mint bármi `pos_sim`-re épülő ág: különben a min_slice elutasításnál
                // soha nem futna le a pozícióellenőrzés (22:17-es „szellem létra” 60 mp után).
                match get_user_state_retry_ok(&rest_client_t, hl_user_t.as_str()).await {
                    Ok(st) => {
                        let ex_pos = clearinghouse_coin_szi(&st, coin_signal.as_str());
                        *pos_sim_t.lock().await = ex_pos;
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Szignál-létra kihagyva: {} (nem küldünk létrát biztonsági okból)",
                            e
                        );
                        continue;
                    }
                }

                let current_pos = *pos_sim_t.lock().await;
                let is_reducing = (current_pos.abs() > 0.001)
                    && ((current_pos > 0.0 && signal.side == "Sell")
                        || (current_pos < 0.0 && signal.side == "Buy"));

                if !is_reducing
                    && (session_start_equity - *wallet_equity_t.lock().await)
                        >= (session_start_equity * 0.10)
                {
                    continue;
                }
                if strategy_for_signal.max_positions == 1
                    && current_pos.abs() > 0.001
                    && !is_reducing
                {
                    continue;
                }

                // Pozícióban: nincs új maker létra — a TP/SL + dust loop intézi a kilépést (különben dupla exit, felesleges fee)
                if current_pos.abs() > 0.001 {
                    tracing::debug!(
                        "Szignál-létra kihagyva: pozíció {:.4} (élő HL)",
                        current_pos
                    );
                    continue;
                }

                let target_n = *target_notional_t.lock().await;
                let min_slice = strategy_for_signal.min_ladder_slice_usd(target_n);
                if min_slice + 1e-6 < strategy_for_signal.min_ladder_order_notional_usd {
                    tracing::info!(
                        "Szignál kihagyva: legkisebb létraszelet {:.2} USD < min_order {:.2} USD (tőke / balance_pct vs díj)",
                        min_slice,
                        strategy_for_signal.min_ladder_order_notional_usd
                    );
                    continue;
                }

                tracing::debug!(
                    "Szignál-létra indítás: coin={} side={} current_pos={:.4} target_n={:.2} min_slice={:.2} min_order={:.2}",
                    coin_signal,
                    signal.side,
                    current_pos,
                    target_n,
                    min_slice,
                    strategy_for_signal.min_ladder_order_notional_usd
                );

                let fe_orders = match get_frontend_open_orders_retry_ok(&rest_client_t, hl_user_t.as_str()).await {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!(
                            "Szignál-létra kihagyva: frontendOpenOrders hiba (nem küldünk létra-vakban): {}",
                            e
                        );
                        continue;
                    }
                };

                let (coin_open_cnt, coin_protected_cnt) = {
                    let mut open_cnt = 0u32;
                    let mut protected_cnt = 0u32;
                    if let Some(arr) = fe_orders.as_array() {
                        for o in arr {
                            if o["coin"].as_str() == Some(coin_signal.as_str()) {
                                open_cnt += 1;
                                if hl_order_is_protected(o) {
                                    protected_cnt += 1;
                                }
                            }
                        }
                    }
                    (open_cnt, protected_cnt)
                };

                tracing::debug!(
                    "Szignál-létra: könyv snapshot coin={} open={} protected={}",
                    coin_signal,
                    coin_open_cnt,
                    coin_protected_cnt
                );
                if frontend_has_blocking_orders_for_coin(&fe_orders, coin_signal.as_str()) {
                    tracing::info!(
                        "Szignál-létra kihagyva: TP/SL, trigger vagy reduce-only order a könyvön"
                    );
                    continue;
                }

                // Feed OID = utolsó batch válaszból; ha nem üres, régen kihagytuk a frontend uniót,
                // és a feedben nem szereplő (szellem) limit soha nem került cancelre → dupla exit.
                let mut cancel_oids = {
                    let mut t = feed_t.open_order_oids.lock().await;
                    let o = t.clone();
                    t.clear();
                    o
                };
                for oid in collect_ladder_cancel_oids_from_frontend(&fe_orders, &coin_signal) {
                    if !cancel_oids.contains(&oid) {
                        cancel_oids.push(oid);
                    }
                }
                cancel_oids =
                    filter_cancel_oids_excluding_position_tpsl_triggers(&fe_orders, &coin_signal, cancel_oids);

                tracing::debug!(
                    "Szignál-létra-cancel: coin={} cancel_oids={}",
                    coin_signal,
                    cancel_oids.len()
                );
                if !cancel_oids.is_empty() {
                    let c_action = order_manager.build_cancel_payload(&cancel_oids);
                    l1_gate_t
                        .run(|nonce| {
                            let signer = signer_t.clone();
                            let rest = rest_client_t.clone();
                            let net = is_mainnet_for_signal;
                            async move {
                                match signer.sign_l1_action(&c_action, nonce, net).await {
                                    Ok(sig) => {
                                        let res = rest.send_l1_action(&c_action, nonce, sig).await;
                                        let _ = exchange_action_ok_or_warn("szignál létra-cancel", &res);
                                    }
                                    Err(e) => tracing::warn!("szignál létra-cancel aláírás: {}", e),
                                }
                            }
                        })
                        .await;
                    tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;
                }

                // A reconcile párhuzamosan tehet ki TP/SL-t; a cancel közben lejárt → új pillanatkép kötelező.
                let fe2 = match get_frontend_open_orders_retry_ok(&rest_client_t, hl_user_t.as_str()).await {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!(
                            "Létra megállítva: frontendOpenOrders (2. körben) hiba — nem küldünk batch-et: {}",
                            e
                        );
                        continue;
                    }
                };

                let (fe2_open_cnt, fe2_protected_cnt) = {
                    let mut open_cnt = 0u32;
                    let mut protected_cnt = 0u32;
                    if let Some(arr) = fe2.as_array() {
                        for o in arr {
                            if o["coin"].as_str() == Some(coin_signal.as_str()) {
                                open_cnt += 1;
                                if hl_order_is_protected(o) {
                                    protected_cnt += 1;
                                }
                            }
                        }
                    }
                    (open_cnt, protected_cnt)
                };

                tracing::debug!(
                    "Szignál-létra: cancel után könyv snapshot coin={} open={} protected={}",
                    coin_signal,
                    fe2_open_cnt,
                    fe2_protected_cnt
                );
                if frontend_has_blocking_orders_for_coin(&fe2, coin_signal.as_str()) {
                    tracing::warn!(
                        "Létra megállítva: könyv változott (TP/SL vagy reduce-only) a cancel után — nem küldünk új batch-et"
                    );
                    continue;
                }
                if frontend_has_any_open_order_for_coin(&fe2, coin_signal.as_str()) {
                    tracing::warn!(
                        "Létra megállítva: {} könyvén maradt nyitott order a létra-cancel után (pl. TP/SL rossz API flaggel) — nem küldünk új batch-et",
                        coin_signal
                    );
                    continue;
                }
                let st2 = match get_user_state_retry_ok(&rest_client_t, hl_user_t.as_str()).await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(
                            "Létra megállítva: get_user_state (2. kör) — {} — nem küldünk batch-et",
                            e
                        );
                        continue;
                    }
                };
                let p2 = clearinghouse_coin_szi(&st2, coin_signal.as_str());
                if p2.abs() > 0.0001 {
                    tracing::warn!(
                        "Létra megállítva: HL pozíció {:.4} jelent meg a cancel után (race a reconcile-dal)",
                        p2
                    );
                    *pos_sim_t.lock().await = p2;
                    continue;
                }

                tracing::debug!(
                    "Szignál-létra: cancel után stabil HL pozíció p2={:.4}",
                    p2
                );

                let s = state_t.read().await;
                let (bid, ask, mid) = (
                    s.best_bid,
                    s.best_ask,
                    if s.best_bid > 0.0 {
                        (s.best_bid + s.best_ask) / 2.0
                    } else {
                        signal.target_mid
                    },
                );
                if bid <= 0.0 {
                    continue;
                }

                order_manager.current_pos = current_pos;

                let action = order_manager.build_ladder_payload(
                    &signal.side,
                    mid,
                    bid,
                    ask,
                    target_n,
                );
                if action.orders.is_empty() {
                    tracing::warn!("Létra üres (min notional / szűrők) — nem küldünk order actiont");
                    continue;
                }

                // Végső pillanat: L1 gate alatt (más L1 nem futhat) clearinghouse + könyv — különben TOCTOU a reconcile ellen.
                let ladder_ok = l1_gate_t
                    .run(|nonce| {
                        let signer = signer_t.clone();
                        let rest = rest_client_t.clone();
                        let feed = feed_t.clone();
                        let pos_sim = pos_sim_t.clone();
                        let act = action.clone();
                        let net = is_mainnet_for_signal;
                        let user = hl_user_t.clone();
                        let coin = coin_signal.clone();
                        async move {
                            let gate_ok =
                                ladder_gate_flat_ok(&rest, user.as_str(), coin.as_str(), &pos_sim).await;
                            if !gate_ok {
                                let ps = *pos_sim.lock().await;
                                tracing::debug!("ladder_gate_flat_ok=false; pos_sim now={:.4}", ps);
                                return false;
                            }
                            match signer.sign_l1_action(&act, nonce, net).await {
                                Ok(sig) => {
                                    let res = rest.send_l1_action(&act, nonce, sig).await;
                                    match exchange_order_ack_or_warn("szignál létra", &res) {
                                        OrderAck::Confirmed => {
                                            if let Ok(ref body) = res {
                                                let new_oids =
                                                    collect_resting_oids_from_exchange_response(body);
                                                if !new_oids.is_empty() {
                                                    feed.open_order_oids.lock().await.extend(new_oids);
                                                }
                                            }
                                            true
                                        }
                                        OrderAck::Uncertain => {
                                            let fe_now =
                                                get_frontend_open_orders_retry_ok_fast(&rest, user.as_str()).await.ok();
                                            let state_now =
                                                get_user_state_retry_ok_fast(&rest, user.as_str()).await.ok();
                                            let has_coin_orders = fe_now
                                                .as_ref()
                                                .map(|fe| frontend_has_any_open_order_for_coin(fe, coin.as_str()))
                                                .unwrap_or(false);
                                            let has_pos = state_now
                                                .as_ref()
                                                .map(|st| clearinghouse_coin_szi(st, coin.as_str()).abs() > 0.0001)
                                                .unwrap_or(false);
                                            if has_coin_orders || has_pos {
                                                tracing::info!(
                                                    "szignál létra: Null válasz után a könyv/pozíció már változott — sikernek tekintjük"
                                                );
                                                true
                                            } else {
                                                false
                                            }
                                        }
                                        OrderAck::Failed => false,
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("szignál létra aláírás: {}", e);
                                    false
                                }
                            }
                        }
                    })
                    .await;

                if ladder_ok {
                    last_signal_time = std::time::Instant::now();
                    info!("🚨 SZIGNÁL: {} @ {:.4}", signal.side, signal.target_mid);
                    *vol_sim_t.lock().await = signal.volatility;
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
        }
    });

    let (signer_r, rest_client_r, pos_rec_t, state_rec, vol_rec_t, hl_user_r, coin_rec, l1_gate_r) =
        (signer.clone(), rest_client.clone(), current_position.clone(), state_ref.clone(), last_volatility.clone(), hl_user.clone(), coin.clone(), l1_gate.clone());
    let om_rec = Arc::new(OrderManager::new(app_config.strategy.clone(), asset_idx, sz_decimals));
    let trade_cooldown_r = trade_cooldown.clone();
    let feed_r = feed.clone();

    tokio::spawn(async move {
        let dust_limit_usd = app_config.strategy.dust_limit_usd;
        let mut last_protected_pos: f64 = 0.0;
        let mut next_dust_ioc_after =
            std::time::Instant::now() - std::time::Duration::from_secs(3600);
        let mut dust_fail_streak: u32 = 0;
        let mut dust_episode_info_logged: bool = false;
        // TP/SL elhelyezés backoff: Null / hálózati hiba esetén ne zaklassuk 150ms-enként a HL-t.
        let mut next_tpsl_after =
            std::time::Instant::now() - std::time::Duration::from_secs(3600);
        let mut tpsl_fail_streak: u32 = 0;

        // Fill-esemény figyelő: azonnali reconcile aktiváláshoz
        let mut fill_rx = feed_r.fill_tx.subscribe();
        // P&L nyomkövetés a cooldown logikához
        let mut prev_ex_pos: f64 = 0.0;
        let mut prev_ent_px: f64 = 0.0;
        let mut last_fill_px: Option<f64> = None;

        loop {
            // Eseményvezérelt trigger: fill WS üzenet OR 150ms timer
            tokio::select! {
                _ = tokio::time::sleep(tokio::time::Duration::from_millis(150)) => {}
                fill_res = fill_rx.recv() => {
                    match fill_res {
                        Ok(fill) if fill.coin == coin_rec => {
                            last_fill_px = Some(fill.px);
                            tracing::debug!(
                                "Fill esemény fogadva: px={:.4} sz={:.4} side={} — azonnali reconcile",
                                fill.px, fill.sz, fill.side
                            );
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("Fill broadcast lemaradt {} üzenettel", n);
                        }
                        _ => {}
                    }
                }
            }

            let st = match get_user_state_retry_ok(&rest_client_r, hl_user_r.as_str()).await {
                Ok(st) => st,
                Err(e) => {
                    tracing::warn!("reconcile: get_user_state {}", e);
                    continue;
                }
            };
                let (ex_pos, ent_px) = clearinghouse_position_for_coin(&st, &coin_rec);
                *pos_rec_t.lock().await = ex_pos;

                // Restart-seed: ha az első ticken már van pozíció (pl. újraindítás közben nyílt),
                // last_fill_px-t az entry price-ra állítjuk — különben a zárás iránya sosem
                // derül ki (last_fill_px=None → cooldown nem triggerel veszteségnél).
                if prev_ex_pos.abs() < 0.001 && ex_pos.abs() > 0.001 && last_fill_px.is_none() {
                    if let Some(ep) = ent_px {
                        last_fill_px = Some(ep);
                        tracing::debug!("Restart-seed: last_fill_px = ent_px {:.4}", ep);
                    }
                }

                // P&L nyomkövetés: ha volt pozíció és most nincs → zárás detektálva
                let was_in_position = prev_ex_pos.abs() > 0.001;
                let now_flat = ex_pos.abs() < 0.001;
                if was_in_position && now_flat && prev_ent_px > 0.0 {
                    if let Some(close_px) = last_fill_px {
                        let prev_long = prev_ex_pos > 0.0;
                        let is_loss = if prev_long { close_px < prev_ent_px } else { close_px > prev_ent_px };
                        if is_loss {
                            trade_cooldown_r.lock().await.on_loss();
                        } else {
                            trade_cooldown_r.lock().await.on_profit();
                        }
                    }
                    last_fill_px = None;
                }
                // Pozíció-tracking frissítése
                if ex_pos.abs() > 0.001 {
                    prev_ent_px = ent_px.unwrap_or(prev_ent_px);
                } else {
                    prev_ent_px = 0.0;
                }
                prev_ex_pos = ex_pos;

                let fe = match get_frontend_open_orders_retry_ok(&rest_client_r, hl_user_r.as_str()).await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!("reconcile: frontendOpenOrders {}", e);
                        continue;
                    }
                };
                let ref_px = ent_px.unwrap_or(state_rec.read().await.mid_price);
                let notional_val = ex_pos.abs() * ref_px;

                if ex_pos.abs() <= 0.0001 || notional_val >= dust_limit_usd {
                    dust_episode_info_logged = false;
                    dust_fail_streak = 0;
                }

                // 🚨 1. DUST CLOSER: Ha a pozíció túl kicsi (dust_limit_usd alatt), bezárja és töröl minden ordert
                if ex_pos.abs() > 0.0001 && notional_val < dust_limit_usd {
                    // Ha már van TP/SL vagy reduce-only kilépő order, ne kavarjunk bele:
                    // a dust ág különben leszedheti a védelmet és új "futyi" limit ciklust okozhat.
                    if frontend_has_blocking_orders_for_coin(&fe, &coin_rec) {
                        tracing::debug!(
                            "Dust closer kihagyva: van blocking order (TP/SL vagy reduce-only) coin={}",
                            coin_rec
                        );
                        continue;
                    }

                    let book = state_rec.read().await;
                    let (bid, ask) = (book.best_bid, book.best_ask);
                    if bid <= 0.0 || ask <= 0.0 || ask <= bid {
                        continue;
                    }
                    let now = std::time::Instant::now();
                    if now < next_dust_ioc_after {
                        continue;
                    }
                    // IOC: limit a könyv szélén (bid eladásnál, ask vételnél); entry/mid gyakran reject
                    let ioc_px = if ex_pos > 0.0 { bid } else { ask };
                    if !dust_episode_info_logged {
                        info!(
                            "🗑️ Dust detektálva (${:.2} < {:.2} USD limit). IOC zárás ex_pos={:.6} bid={:.4} ask={:.4}",
                            notional_val, dust_limit_usd, ex_pos, bid, ask
                        );
                        dust_episode_info_logged = true;
                    } else {
                        tracing::debug!(
                            "Dust ismétlődő IOC próba: notional=${:.2} ex_pos={:.6} fail_streak={}",
                            notional_val,
                            ex_pos,
                            dust_fail_streak
                        );
                    }
                    let close_action = om_rec.build_market_close_payload(ex_pos < 0.0, ioc_px, ex_pos.abs());
                    let close_ok = l1_gate_r
                        .run(|nonce| {
                            let s = signer_r.clone();
                            let r = rest_client_r.clone();
                            let net = app_config.is_mainnet;
                            async move {
                                match s.sign_l1_action(&close_action, nonce, net).await {
                                    Ok(sig) => {
                                        let res = r.send_l1_action(&close_action, nonce, sig).await;
                                        exchange_action_ok_or_warn("reconcile dust IOC zárás", &res)
                                    }
                                    Err(e) => {
                                        tracing::warn!("dust zárás aláírás: {}", e);
                                        false
                                    }
                                }
                            }
                        })
                        .await;

                    if close_ok {
                        dust_fail_streak = 0;
                        next_dust_ioc_after = now + std::time::Duration::from_secs(3);
                    } else {
                        dust_fail_streak = dust_fail_streak.saturating_add(1);
                        let backoff_secs =
                            (3u64.saturating_mul(1u64 << dust_fail_streak.min(5))).min(120);
                        next_dust_ioc_after = now + std::time::Duration::from_secs(backoff_secs);
                        // A HL hiba részleteit az `exchange_action_ok_or_warn` már warnolja.
                        tracing::debug!(
                            "Dust IOC sikertelen: streak={} következő próba ~{}s múlva",
                            dust_fail_streak,
                            backoff_secs
                        );
                    }

                    // Dust után csak "létra" maradékot cancelünk; protected/reduce-only maradjon békén.
                    let cancel_oids = collect_ladder_cancel_oids_from_frontend(&fe, &coin_rec);
                    if !cancel_oids.is_empty() {
                        let cancel_a = om_rec.build_cancel_payload(&cancel_oids);
                        l1_gate_r
                            .run(|nonce| {
                                let s = signer_r.clone();
                                let r = rest_client_r.clone();
                                let net = app_config.is_mainnet;
                                async move {
                                    match s.sign_l1_action(&cancel_a, nonce, net).await {
                                        Ok(sig) => {
                                            let res = r.send_l1_action(&cancel_a, nonce, sig).await;
                                            let _ = exchange_action_ok_or_warn("reconcile dust utáni cancel", &res);
                                        }
                                        Err(e) => tracing::warn!("dust cancel aláírás: {}", e),
                                    }
                                }
                            })
                            .await;
                    }
                    last_protected_pos = 0.0;
                    continue;
                }

                // 🧹 2. TAKARÍTÁS HA NINCS POZÍCIÓ
                if ex_pos.abs() < 0.0001 {
                    if last_protected_pos.abs() > 0.0001 {
                        let oids: Vec<u64> = fe.as_array().unwrap_or(&vec![]).iter()
                            .filter(|o| o["coin"].as_str() == Some(&coin_rec))
                            .filter_map(|o| o["oid"].as_u64().or_else(|| o["oid"].as_str().and_then(|v| v.parse().ok()))).collect();
                        if !oids.is_empty() {
                            let action = om_rec.build_cancel_payload(&oids);
                            l1_gate_r
                                .run(|nonce| {
                                    let s = signer_r.clone();
                                    let r = rest_client_r.clone();
                                    let net = app_config.is_mainnet;
                                    async move {
                                        match s.sign_l1_action(&action, nonce, net).await {
                                            Ok(sig) => {
                                                let res = r.send_l1_action(&action, nonce, sig).await;
                                                let _ = exchange_action_ok_or_warn("reconcile flat takarítás cancel", &res);
                                            }
                                            Err(e) => tracing::warn!("flat cancel aláírás: {}", e),
                                        }
                                    }
                                })
                                .await;
                        }
                    }
                    last_protected_pos = 0.0; continue;
                }

                // 🛡️ 3. ANTI-STALE LADDER (Csak TP/SL trigger maradhat)
                // Korábban !reduceOnly szűrő miatt a maker „szellem” limit (reduce-only, nem trigger) nem törlődött.
                if ex_pos.abs() > 0.0001 {
                    let stale = collect_ladder_cancel_oids_from_frontend(&fe, &coin_rec);

                    tracing::debug!(
                        "reconcile anti-stale: coin={} ex_pos={:.4} last_protected_pos={:.4} stale_oids={}",
                        coin_rec,
                        ex_pos,
                        last_protected_pos,
                        stale.len()
                    );
                    if !stale.is_empty() {
                        tracing::debug!("reconcile anti-stale: cancelling stale_oids={}", stale.len());
                        let action = om_rec.build_cancel_payload(&stale);
                        l1_gate_r
                            .run(|nonce| {
                                let s = signer_r.clone();
                                let r = rest_client_r.clone();
                                let net = app_config.is_mainnet;
                                async move {
                                    match s.sign_l1_action(&action, nonce, net).await {
                                        Ok(sig) => {
                                            let res = r.send_l1_action(&action, nonce, sig).await;
                                            let _ = exchange_action_ok_or_warn("reconcile stale létra cancel", &res);
                                        }
                                        Err(e) => tracing::warn!("stale cancel aláírás: {}", e),
                                    }
                                }
                            })
                            .await;
                    }

                    let protected_count = fe
                        .as_array()
                        .unwrap_or(&vec![])
                        .iter()
                        .filter(|o| o["coin"].as_str() == Some(&coin_rec) && hl_order_is_protected(o))
                        .count();
                    let has_tpsl = protected_count > 0;
                    let delta = (ex_pos.abs() - last_protected_pos.abs()).abs();
                    tracing::debug!(
                        "reconcile anti-stale: has_tpsl={} protected_count={} delta_from_last_protected={:.6}",
                        has_tpsl,
                        protected_count,
                        delta
                    );
                    // Ha TP/SL már kint van és a pozíció nem változott lényegesen → nincs teendő.
                    // Emellett: ha last_protected_pos=0 (restart vagy Null-válasz utáni állapot),
                    // de a TP/SL már fent van a könyvön, szinkronizálunk és nem cancelünk/küldünk újra.
                    if protected_count == 2 && (delta < 0.001 || last_protected_pos.abs() < 0.001) {
                        last_protected_pos = ex_pos; // tracking szinkronizálása (restart + Null után is)
                        tpsl_fail_streak = 0;
                        continue;
                    }
                }

                if (ex_pos.abs() - last_protected_pos.abs()).abs() < 0.001 { continue; }

                if ref_px <= 0.0 { continue; }

                // Régi TP/SL törlése MIELŐTT új kerülne ki — különben restart/részleges zárás után
                // dupla TP/SL halmozódik a könyvön.
                let mut cancel_was_uncertain = false;
                {
                    let old_tpsl: Vec<u64> = fe.as_array().unwrap_or(&vec![]).iter()
                        .filter(|o| o["coin"].as_str() == Some(&coin_rec) && hl_order_is_protected(o))
                        .filter_map(|o| o["oid"].as_u64()
                            .or_else(|| o["oid"].as_str().and_then(|v| v.parse().ok())))
                        .collect();
                    if !old_tpsl.is_empty() {
                        info!("🧹 Régi TP/SL törlése ({} db) újraküldés előtt...", old_tpsl.len());
                        let cancel_old = om_rec.build_cancel_payload(&old_tpsl);
                        let cancel_ack = l1_gate_r
                            .run(|nonce| {
                                let s = signer_r.clone();
                                let r = rest_client_r.clone();
                                let net = app_config.is_mainnet;
                                async move {
                                    match s.sign_l1_action(&cancel_old, nonce, net).await {
                                        Ok(sig) => {
                                            let res = r.send_l1_action(&cancel_old, nonce, sig).await;
                                            exchange_cancel_ack_or_warn("régi TP/SL cancel", &res)
                                        }
                                        Err(e) => {
                                            tracing::warn!("régi TP/SL cancel aláírás: {}", e);
                                            CancelAck::Failed
                                        }
                                    }
                                }
                            })
                            .await;
                        cancel_was_uncertain = cancel_ack == CancelAck::Uncertain;
                        tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;
                    }
                }

                // Cancel után kötelezően újraolvassuk a könyvet. Ez megfogja a bizonytalan (`Null`)
                // cancel-választ és megakadályozza a dupla TP/SL újraküldést.
                let fe_after_cancel = match get_frontend_open_orders_retry_ok(&rest_client_r, hl_user_r.as_str()).await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!("reconcile: cancel utáni frontendOpenOrders {}", e);
                        continue;
                    }
                };
                let protected_after_cancel = fe_after_cancel
                    .as_array()
                    .unwrap_or(&vec![])
                    .iter()
                    .filter(|o| o["coin"].as_str() == Some(&coin_rec) && hl_order_is_protected(o))
                    .count();
                if protected_after_cancel > 0 {
                    last_protected_pos = ex_pos;
                    tpsl_fail_streak = 0;
                    if cancel_was_uncertain {
                        tracing::info!("reconcile: TP/SL cancel bizonytalan volt, de a védelem még a könyvön van — nem küldünk új TP/SL-t");
                    }
                    continue;
                }

                // 🚨 VÉSZLEÁLLÍTÁS: tartósan sikertelen TP/SL → kényszerzárás piaci áron.
                // Ha 8 egymást követő ticken sem sikerül TP/SL-t felrakni (Null válasz, hálózat,
                // ár-számítás), a bot IOC-cal zárja a pozíciót — különben napokig „ragad" TP/SL nélkül.
                if tpsl_fail_streak >= 8 {
                    let (bid, ask) = {
                        let book = state_rec.read().await;
                        (book.best_bid, book.best_ask)
                    };
                    if bid > 0.0 && ask > 0.0 && ask > bid {
                        tracing::error!(
                            "🚨 VÉSZLEÁLLÍTÁS: TP/SL {}x sikertelen — kényszerzárás ex_pos={:.4}",
                            tpsl_fail_streak, ex_pos
                        );
                        let ioc_px = if ex_pos > 0.0 { bid } else { ask };
                        let close_action = om_rec.build_market_close_payload(ex_pos < 0.0, ioc_px, ex_pos.abs());
                        let closed = l1_gate_r
                            .run(|nonce| {
                                let s = signer_r.clone();
                                let r = rest_client_r.clone();
                                let net = app_config.is_mainnet;
                                async move {
                                    match s.sign_l1_action(&close_action, nonce, net).await {
                                        Ok(sig) => {
                                            let res = r.send_l1_action(&close_action, nonce, sig).await;
                                            exchange_action_ok_or_warn("vészleállítás IOC zárás", &res)
                                        }
                                        Err(e) => {
                                            tracing::warn!("vészleállítás aláírás: {}", e);
                                            false
                                        }
                                    }
                                }
                            })
                            .await;
                        tpsl_fail_streak = 0;
                        last_protected_pos = 0.0;
                        next_tpsl_after = std::time::Instant::now()
                            + std::time::Duration::from_secs(if closed { 5 } else { 30 });
                    } else {
                        tracing::error!(
                            "🚨 VÉSZLEÁLLÍTÁS: {}x TP/SL hiba, nincs érvényes bid/ask — következő tick",
                            tpsl_fail_streak
                        );
                    }
                    continue;
                }

                // TP/SL újrapróba backoff: Null / hálózati hiba esetén ne zaklassuk 150ms-enként.
                {
                    let now = std::time::Instant::now();
                    if now < next_tpsl_after {
                        tracing::debug!(
                            "TP/SL újrapróba halasztva — {:.0}s múlva (streak={})",
                            (next_tpsl_after - now).as_secs_f64(),
                            tpsl_fail_streak
                        );
                        continue;
                    }
                }

                // Ha a feed átmenetileg halott (mid_price=0), az entry price alapján számolunk TP/SL-t;
                // jobb egy konzervatív védelem, mint hogy a bot TP/SL nélkül ragadjon napokig.
                let mark_mid_for_tpsl = {
                    let m = state_rec.read().await.mid_price;
                    if m > 0.0 { m } else { ref_px }
                };
                match OrderManager::tp_sl_prices_for_position(ex_pos, ref_px, *vol_rec_t.lock().await, app_config.strategy.min_tick_size, app_config.strategy.tp_min_pct, app_config.strategy.sl_min_pct, mark_mid_for_tpsl, app_config.strategy.maker_fee_rate, app_config.strategy.taker_fee_rate) {
                    None => {
                        tpsl_fail_streak = tpsl_fail_streak.saturating_add(1);
                        let backoff_secs = (5u64.saturating_mul(1u64 << tpsl_fail_streak.min(4))).min(60);
                        next_tpsl_after = std::time::Instant::now() + std::time::Duration::from_secs(backoff_secs);
                        tracing::warn!(
                            "TP/SL számítás None (streak={}): mark={:.4} ref={:.4} — {}s backoff",
                            tpsl_fail_streak, mark_mid_for_tpsl, ref_px, backoff_secs
                        );
                    }
                    Some((tp, sl)) => {
                    tracing::debug!(
                        "reconcile TP/SL: coin={} ex_pos={:.4} ref_px={:.4} tp={:.4} sl={:.4}",
                        coin_rec,
                        ex_pos,
                        ref_px,
                        tp,
                        sl
                    );
                    let prot = om_rec.build_protective_tpsl_payload(if ex_pos > 0.0 { "Sell" } else { "Buy" }, tp, sl, ex_pos.abs());
                    let ack = l1_gate_r
                        .run(|nonce| {
                            let s = signer_r.clone();
                            let r = rest_client_r.clone();
                            let net = app_config.is_mainnet;
                            async move {
                                match s.sign_l1_action(&prot, nonce, net).await {
                                    Ok(sig) => {
                                        let res = r.send_l1_action(&prot, nonce, sig).await;
                                        exchange_order_ack_or_warn("reconcile TP/SL", &res)
                                    }
                                    Err(e) => {
                                        tracing::warn!("TP/SL aláírás: {}", e);
                                        OrderAck::Failed
                                    }
                                }
                            }
                        })
                        .await;

                    // Null válasz esetén a HL gyakran mégis felteszi a TP/SL-t — könyv-ellenőrzéssel
                    // igazoljuk, ne hamis "sikertelen" miatt halmozódjon duplikátum (5 TP/SL eset).
                    let ok = match ack {
                        OrderAck::Confirmed => true,
                        OrderAck::Failed => false,
                        OrderAck::Uncertain => {
                            tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
                            match get_frontend_open_orders_retry_ok_fast(&rest_client_r, hl_user_r.as_str()).await {
                                Ok(fe_verify) => {
                                    let protected_now = fe_verify
                                        .as_array()
                                        .unwrap_or(&vec![])
                                        .iter()
                                        .filter(|o| o["coin"].as_str() == Some(&coin_rec) && hl_order_is_protected(o))
                                        .count();
                                    if protected_now >= 2 {
                                        tracing::info!(
                                            "reconcile TP/SL: Null válasz, de {} védő order a könyvön — sikernek tekintjük",
                                            protected_now
                                        );
                                        true
                                    } else {
                                        tracing::warn!(
                                            "reconcile TP/SL: Null válasz és csak {} védő order kint — sikertelennek vesszük",
                                            protected_now
                                        );
                                        false
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("reconcile TP/SL: Null utáni könyv-ellenőrzés hibás: {} — sikertelennek vesszük", e);
                                    false
                                }
                            }
                        }
                    };
                    if ok {
                        last_protected_pos = ex_pos;
                        // Részleges fill után a régi maker létra maradék gyakran csak TP/SL feltevése után
                        // látszik a könyvben; egy friss olvasás + sweep megszünteti a dupla kitettséget.
                        match get_frontend_open_orders_retry_ok(&rest_client_r, hl_user_r.as_str()).await {
                            Ok(fe_sweep) => {
                                let sweep =
                                    collect_ladder_cancel_oids_from_frontend(&fe_sweep, &coin_rec);
                                if !sweep.is_empty() {
                                    tracing::info!(
                                        "reconcile: TP/SL utáni létra-sweep — {} maradék maker oid",
                                        sweep.len()
                                    );
                                    let cancel_a = om_rec.build_cancel_payload(&sweep);
                                    l1_gate_r
                                        .run(|nonce| {
                                            let s = signer_r.clone();
                                            let r = rest_client_r.clone();
                                            let net = app_config.is_mainnet;
                                            async move {
                                                match s.sign_l1_action(&cancel_a, nonce, net).await {
                                                    Ok(sig) => {
                                                        let res =
                                                            r.send_l1_action(&cancel_a, nonce, sig).await;
                                                        let _ = exchange_action_ok_or_warn(
                                                            "reconcile TP/SL utáni létra-sweep cancel",
                                                            &res,
                                                        );
                                                    }
                                                    Err(e) => tracing::warn!(
                                                        "TP/SL utáni sweep cancel aláírás: {}",
                                                        e
                                                    ),
                                                }
                                            }
                                        })
                                        .await;
                                }
                            }
                            Err(e) => tracing::warn!(
                                "reconcile: TP/SL utáni sweep — frontendOpenOrders {}",
                                e
                            ),
                        }
                        tpsl_fail_streak = 0;
                    } else {
                        // TP/SL elhelyezés sikertelen (Null válasz vagy hálózati hiba).
                        // A HL néha Null-t ad vissza, de az order mégis teljesül — a has_tpsl
                        // check a következő tickben észleli és szinkronizálja last_protected_pos-t.
                        // Ha az order valóban nem ment ki, a backoff megakadályozza a 150ms-es hammert.
                        tpsl_fail_streak = tpsl_fail_streak.saturating_add(1);
                        let backoff_secs =
                            (5u64.saturating_mul(1u64 << tpsl_fail_streak.min(4))).min(60);
                        next_tpsl_after = std::time::Instant::now()
                            + std::time::Duration::from_secs(backoff_secs);
                        tracing::warn!(
                            "TP/SL elhelyezés sikertelen (streak={}): {}s múlva újrapróba (ha nincs kint TP/SL)",
                            tpsl_fail_streak,
                            backoff_secs
                        );
                    }
                    }
                }
        }
    });

    tokio::signal::ctrl_c().await?;
    Ok(())
}

/// Két körben (120 ms között): clearinghouse lapos + nincs nyitott order a coinon.
/// A HL `frontendOpenOrders` néha egy pillanatig nem mutatja a friss TP/SL-t — egy olvasás alatt átcsúszhat a maker létra.
async fn ladder_gate_flat_ok(
    rest: &HyperliquidClient,
    user: &str,
    coin: &str,
    pos_sim: &Arc<tokio::sync::Mutex<f64>>,
) -> bool {
    for pass in 0..2 {
        tracing::debug!("ladder_gate_flat_ok pass {}: user={} coin={}", pass + 1, user, coin);
        if pass == 1 {
            tokio::time::sleep(tokio::time::Duration::from_millis(120)).await;
        }
        match get_user_state_retry_ok_fast(rest, user).await {
            Ok(st) => {
                let p = clearinghouse_coin_szi(&st, coin);
                if p.abs() > 0.0001 {
                    tracing::warn!(
                        "létra (L1 gate pass {}): pozíció {:.4} — nem küldünk batch-et",
                        pass + 1,
                        p
                    );
                    *pos_sim.lock().await = p;
                    return false;
                }
            }
            Err(e) => {
                tracing::warn!("létra (L1 gate pass {}): get_user_state {}", pass + 1, e);
                return false;
            }
        }
        match get_frontend_open_orders_retry_ok_fast(rest, user).await {
            Ok(fe) => {
                let (open_cnt, protected_cnt) = {
                    let mut open_cnt = 0u32;
                    let mut protected_cnt = 0u32;
                    if let Some(arr) = fe.as_array() {
                        for o in arr {
                            if o["coin"].as_str() == Some(coin) {
                                open_cnt += 1;
                                if hl_order_is_protected(o) {
                                    protected_cnt += 1;
                                }
                            }
                        }
                    }
                    (open_cnt, protected_cnt)
                };
                let blocking = frontend_has_blocking_orders_for_coin(&fe, coin);
                let any_open = frontend_has_any_open_order_for_coin(&fe, coin);
                if blocking || any_open {
                    tracing::warn!(
                        "létra (L1 gate pass {}): könyv nem flat (coin={}) — open={} protected={} blocking={} any_open={} — nem küldünk batch-et",
                        pass + 1,
                        coin,
                        open_cnt,
                        protected_cnt,
                        blocking,
                        any_open
                    );
                    return false;
                }
            }
            Err(e) => {
                tracing::warn!(
                    "létra (L1 gate pass {}): frontendOpenOrders {}",
                    pass + 1,
                    e
                );
                return false;
            }
        }
    }
    true
}
