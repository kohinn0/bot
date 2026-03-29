mod config;
mod network;
mod logic;

use dotenvy::dotenv;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use crate::config::AppConfig;
use crate::logic::signer::HyperliquidSigner;
use crate::logic::signal::SignalEngine;
use crate::logic::order_manager::OrderManager;
use crate::network::client::{
 collect_ladder_cancel_oids_from_frontend, collect_ladder_cancel_oids_from_open_orders,
 cancel_response_only_benign_errors, collect_resting_oids_from_exchange_response,
 exchange_order_submission_ok, exchange_response_has_post_only_reject,
 filter_cancel_oids_excluding_position_tpsl_triggers,
 frontend_position_tpsl_matches_pos,
 collect_position_tpsl_oids,
 HyperliquidClient,
};
use crate::network::feed::HyperliquidFeed;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
 let subscriber = FmtSubscriber::builder()
 .with_max_level(Level::INFO)
 .finish();
 tracing::subscriber::set_global_default(subscriber)
 .expect("Failed to set tracing subscriber");

 dotenv().ok();

 info!(" INICIALIZÁLÁS: SebessegBot v3 (Rust/Tokio) ");

 // 1. Konfiguráció
 let app_config = AppConfig::load();
 let coin = app_config.strategy.coin.clone();

 // 2. Aláíró és Kliens
 let is_mainnet = app_config.is_mainnet;
 let signer = HyperliquidSigner::new(&app_config.private_key);
 let rest_client = HyperliquidClient::new(is_mainnet, app_config.hl_perp_dex.clone());

 let signer_addr = signer.get_address().to_string();
 let hl_user = app_config
 .hl_user_address
 .as_ref()
 .map(|s| s.trim().to_string())
 .filter(|s| !s.is_empty())
 .unwrap_or_else(|| signer_addr.clone());

 info!(" Aláíró cím (PRIVATE_KEY): {}", signer_addr);
 if !hl_user.eq_ignore_ascii_case(&signer_addr) {
 info!(" HL user: {} ← HL_USER_ADDRESS", hl_user);
 } else {
 info!(" HL user = aláíró cím (HL_USER_ADDRESS nincs beállítva)");
 }

 // 3. Asset Meta
 let meta = rest_client.get_meta().await.expect(" Nem sikerült lekérni a meta adatokat");
 let mut asset_idx = 0u32;
 let mut sz_decimals = 0u32;
 if let Some(universe) = meta["universe"].as_array() {
 for (idx, coin_data) in universe.iter().enumerate() {
 if coin_data["name"].as_str().unwrap_or("") == coin {
 asset_idx = idx as u32;
 sz_decimals = coin_data["szDecimals"].as_u64().unwrap_or(0) as u32;
 break;
 }
 }
 }
 info!(" Kereskedési pár: {}, Asset ID: {}, Size Decimals: {}", coin, asset_idx, sz_decimals);

 // 4. WebSocket Feed
 let feed = Arc::new(HyperliquidFeed::new(&coin, &hl_user, is_mainnet));
 let state_ref = feed.state.clone();
 feed.clone().start().await;

 let user_address = hl_user.clone();
 let max_daily_loss_usd = app_config.strategy.max_daily_loss_usd;
 let max_daily_trades = app_config.strategy.max_daily_trades;

 // 5. Egyenleg lekérés
 let use_hl_equity = app_config.use_wallet_balance_for_sizing && !app_config.is_dry_run;
 let (wallet_equity_usd, equity_source): (f64, String) = if use_hl_equity {
 match rest_client.get_account_value_usd(user_address.as_str()).await {
 Ok(v) if v.is_finite() && v > 0.0 => (v, "Hyperliquid perp + spot USDC (API)".to_string()),
 Ok(v) => {
 tracing::warn!(
 " HL API egyenleg ~0 (${:.4}) — fallback STARTING_EQUITY_USD.",
 v
 );
 (app_config.starting_equity_usd, "fallback STARTING_EQUITY_USD (érvénytelen HL)".to_string())
 }
 Err(e) => {
 tracing::warn!(" HL egyenleg lekérés sikertelen: {} — fallback", e);
 (app_config.starting_equity_usd, "fallback STARTING_EQUITY_USD (API hiba)".to_string())
 }
 }
 } else {
 let why = if app_config.is_dry_run { "DRY_RUN" }
 else if !app_config.use_wallet_balance_for_sizing { "USE_WALLET_BALANCE_FOR_SIZING=false" }
 else { "kikapcsolva" };
 (app_config.starting_equity_usd, format!("STARTING_EQUITY_USD ({})", why))
 };

 let session_start_equity = wallet_equity_usd;
 info!(" Számlaérték: ${:.2} — forrás: {}", wallet_equity_usd, equity_source);

 let wallet_equity = Arc::new(tokio::sync::Mutex::new(wallet_equity_usd));
 let initial_notional = app_config.strategy.notional_per_level_usd(wallet_equity_usd);
 let target_notional_usd = Arc::new(tokio::sync::Mutex::new(initial_notional));
 let trade_count = Arc::new(AtomicU32::new(0));
 let current_position = Arc::new(tokio::sync::Mutex::new(0.0f64));
 let last_volatility = Arc::new(tokio::sync::Mutex::new(0.01f64));

 info!(" Kereskedési méret (notional): ${:.2} per szint", initial_notional);

 // Háttér: egyenleg frissítése
 if use_hl_equity {
 let rest_w = rest_client.clone();
 let addr_w = user_address.clone();
 let wallet_w = wallet_equity.clone();
 let target_w = target_notional_usd.clone();
 let strat_w = app_config.strategy.clone();
 let sec = app_config.wallet_equity_refresh_sec.max(10);
 tokio::spawn(async move {
 loop {
 tokio::time::sleep(tokio::time::Duration::from_secs(sec)).await;
 match rest_w.get_account_value_usd(&addr_w).await {
 Ok(v) if v.is_finite() && v > 0.0 => {
 let n = strat_w.notional_per_level_usd(v);
 *wallet_w.lock().await = v;
 *target_w.lock().await = n;
 tracing::debug!(" HL egyenleg frissítve: ${:.2} → notional ${:.2}", v, n);
 }
 Ok(v) => tracing::warn!(" HL accountValue kihagyva (érvénytelen): {}", v),
 Err(e) => tracing::warn!(" HL egyenleg frissítés hiba: {}", e),
 }
 }
 });
 }

 // 6. Signal motor + Order Manager
 let mut signal_engine = SignalEngine::new(app_config.strategy.clone(), state_ref.clone());
 let mut order_manager = OrderManager::new(app_config.strategy.clone(), asset_idx, sz_decimals);
 let is_dry_run = app_config.is_dry_run;

 let signer = Arc::new(signer);
 let rest_client = Arc::new(rest_client);

 // Fill figyelő
 let mut fill_rx = feed.fill_tx.subscribe();
 let trades_t = trade_count.clone();
 let pos_t = current_position.clone();
 tokio::spawn(async move {
 while let Ok(fill) = fill_rx.recv().await {
 trades_t.fetch_add(1, Ordering::Relaxed);
 let pos = *pos_t.lock().await;
 info!(
 " Fill {} @ {} sz={} fee=${:.4} (pos from reconcile: {:.4})",
 fill.coin, fill.px, fill.sz, fill.fee, pos
 );
 }
 });

 // Leverage beállítás
 if !is_dry_run {
 info!(" Tőkeáttétel {}x beállítása...", app_config.strategy.leverage);
 let leverage_action = order_manager.build_leverage_payload();
 let nonce = std::time::SystemTime::now()
 .duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
 match signer.sign_l1_action(&leverage_action, nonce, is_mainnet).await {
 Ok(sig) => match rest_client.send_l1_action(&leverage_action, nonce, sig).await {
 Ok(_) => info!(" Tőkeáttétel beállítva"),
 Err(e) => tracing::error!(" Tőkeáttétel beállítás hiba: {}", e),
 },
 Err(e) => tracing::error!(" Tőkeáttétel aláírás hiba: {}", e),
 }
 }

 info!(" Kereskedési ciklus elindítva...");

 let signer_t = signer.clone();
 let signer_r = signer.clone();
 let feed_t = feed.clone();
 let hl_user_t = hl_user.clone();
 let hl_user_r = hl_user.clone();
 let rest_client_r = rest_client.clone();
 let pos_sim_t = current_position.clone();
 let pos_reconcile_t = current_position.clone();
 let vol_sim_t = last_volatility.clone();
 let vol_reconcile_t = last_volatility.clone();
 let state_t = state_ref.clone();
 let state_reconcile = state_ref.clone();
 let om_reconcile = Arc::new(OrderManager::new(app_config.strategy.clone(), asset_idx, sz_decimals));
 let max_pos_limit = app_config.strategy.max_positions;
 let mut last_signal_time = std::time::Instant::now() - std::time::Duration::from_secs(60);
 let min_signal_interval = std::time::Duration::from_millis(app_config.strategy.min_signal_interval_ms);
 let coin_signal = coin.clone();
 let account_guard_t = wallet_equity.clone();
 let trade_guard_t = trade_count.clone();
 let target_notional_t = target_notional_usd.clone();

 // 7. Fő heartbeat loop
 tokio::spawn(async move {
 let mut last_reset_day: u32 = {
 let now = std::time::SystemTime::now()
 .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
 (now / 86400) as u32
 };
 let mut session_start_equity = session_start_equity;

 loop {
 let today = {
 let now = std::time::SystemTime::now()
 .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
 (now / 86400) as u32
 };
 if today != last_reset_day {
 last_reset_day = today;
 trade_guard_t.store(0, Ordering::Relaxed);
 let eq = *account_guard_t.lock().await;
 session_start_equity = eq;
 info!(" Napi reset (UTC éjfél): trade_count=0, drawdown bázis=${:.2}", eq);
 }

if let Some(signal) = signal_engine.tick().await {
                let current_equity = *account_guard_t.lock().await;
                let drawdown = (session_start_equity - current_equity).max(0.0);
                let profit = (current_equity - session_start_equity).max(0.0);

                // 🚀 1. POZÍCIÓ ELLENŐRZÉSE: Zárni akarunk vagy nyitni?
                let current_pos: f64 = *pos_sim_t.lock().await;
                let is_reducing = (current_pos > 0.001 && signal.side == "Sell")
                    || (current_pos < -0.001 && signal.side == "Buy");

                // 🛡️ 2. KOCKÁZATKEZELÉS: Csak akkor blokkolunk, ha ÚJ pozíciót nyitnánk!
                if !is_reducing {
                    // Maximum 10% napi veszteség
                    let max_loss_pct = 0.10; 
                    let effective_loss_limit = session_start_equity * max_loss_pct;

                    if drawdown >= effective_loss_limit {
                        tracing::error!(
                            "🛑 MAX DRAWDOWN ELÉRVE (-{:.1}% / -${:.2}). Új belépés letiltva!", 
                            max_loss_pct * 100.0, drawdown
                        );
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                        continue;
                    }
                    
                    // Napi 5% profit cél. Ha nem akarod, hogy leálljon nyerőben, ezt a részt kommenteld ki!
                    if profit >= session_start_equity * 0.05 {
                        tracing::info!("✅ DAILY PROFIT TARGET (+5% / +${:.2}), új belépés blokkolva.", profit);
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                        continue;
                    }

                    // ⚠️ A TRADE LIMITET INNEN TELJESEN KIGYOMLÁLTUK. SOSEM FOG LEOKADNI.
                }

                if last_signal_time.elapsed() < min_signal_interval {
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                    continue;
                }

                *vol_sim_t.lock().await = signal.volatility;

                // 🛡️ 3. HARD LIMIT: Ha van pozíciónk, és nem zárni akarunk, kíméletlen stop.
                if max_pos_limit == 1 && current_pos.abs() > 0.001 {
                    if !is_reducing {
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                        continue;
                    }
                }

                info!(
                    "🚨 SZIGNÁL: {} @ {:.4} | z={:.2} | bars={} | vol={:.4} | pos={:.4}",
                    signal.side, signal.target_mid, signal.z_score, signal.bar_count, signal.volatility, current_pos
                );

 order_manager.current_pos = current_pos;

 let max_close_sz = if is_reducing {
 Some(order_manager.quantize_position_sz(current_pos.abs()))
 } else {
 None
 };

 if is_dry_run {
 info!(" DRY RUN: szignál (nincs küldés)");
 let fill_sz = if signal.side == "Buy" { 0.1 } else { -0.1 };
 *pos_sim_t.lock().await += fill_sz;
 continue; // Dry run-nál ne menjünk tovább
 }

 // 1. CLEAN SLATE
 let addr = hl_user_t.as_str();
 let fe_orders = rest_client.get_frontend_open_orders(addr).await.ok();
 let open_orders_basic = if fe_orders.is_none() {
 rest_client.get_open_orders(addr).await.ok()
 } else {
 None
 };

 let mut cancel_oids = {
 let mut tracked = feed_t.open_order_oids.lock().await;
 let oids = tracked.clone();
 tracked.clear();
 oids
 };

 if cancel_oids.is_empty() {
 if let Some(ref fe) = fe_orders {
 cancel_oids = collect_ladder_cancel_oids_from_frontend(fe, &coin_signal);
 } else if let Some(ref oo) = open_orders_basic {
 cancel_oids = collect_ladder_cancel_oids_from_open_orders(oo, &coin_signal);
 }
 }

 if let Some(ref fe) = fe_orders {
 cancel_oids = filter_cancel_oids_excluding_position_tpsl_triggers(fe, &coin_signal, cancel_oids);
 }

 if !cancel_oids.is_empty() {
 let cancel_action = order_manager.build_cancel_payload(&cancel_oids);
 let c_nonce = std::time::SystemTime::now()
 .duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
 if let Ok(sig) = signer_t.sign_l1_action(&cancel_action, c_nonce, is_mainnet).await {
 let _ = rest_client.send_l1_action(&cancel_action, c_nonce, sig).await;
 info!(" SZELLEM-ORDERS TÖRÖLVE ({} db)", cancel_oids.len());
 }
 }

 tokio::time::sleep(tokio::time::Duration::from_millis(15)).await;

 if !feed_t.open_order_oids.lock().await.is_empty() {
 info!(" Új létra kihagyva: még vannak nyitott orderek.");
 continue;
 }

 // 2. ÚJ LÉTRA KIHELYEZÉSE
 let (best_bid, best_ask, ladder_mid) = {
 let s = state_t.read().await;
 let mid = if s.best_bid > 0.0 && s.best_ask > 0.0 {
 (s.best_bid + s.best_ask) / 2.0
 } else {
 signal.target_mid
 };
 (s.best_bid, s.best_ask, mid)
 };

 const HL_MIN_ORDER_NOTIONAL_USD: f64 = 10.05;

 if best_bid <= 0.0 || best_ask <= 0.0 {
 tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
 continue;
 }

 if let Some(close_sz) = max_close_sz {
 let est = close_sz * ladder_mid;
 if est < HL_MIN_ORDER_NOTIONAL_USD {
 info!(" DUST pozíció piaci zárás: {:.4} ≈ ${:.2}", close_sz, est);
 let is_buy_close = current_pos < 0.0;
 let market_px = if is_buy_close {
 (best_ask * 1.01 / order_manager.config_tick()).ceil() * order_manager.config_tick()
 } else {
 (best_bid * 0.99 / order_manager.config_tick()).floor() * order_manager.config_tick()
 };
 let market_action = order_manager.build_market_close_payload(is_buy_close, market_px, close_sz);
 let m_nonce = std::time::SystemTime::now()
 .duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
 if let Ok(sig) = signer_t.sign_l1_action(&market_action, m_nonce, is_mainnet).await {
 let _ = rest_client.send_l1_action(&market_action, m_nonce, sig).await;
 }
 tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
 continue;
 }
 }

 // PRE-FLIGHT VÉDELEM: Atomikus check közvetlenül az építés előtt 
 let final_pos_check = *pos_sim_t.lock().await;
 if max_pos_limit == 1 && final_pos_check.abs() > 0.001 && !is_reducing {
 tracing::warn!(" Kettős védelem megmentett: a háttérben létrejött egy pozíció ({:.4}), új belépés megszakítva!", final_pos_check);
 continue;
 }

 let target_usd = *target_notional_t.lock().await;
 let action = order_manager.build_ladder_payload(
 &signal.side, ladder_mid, best_bid, best_ask, target_usd, max_close_sz,
 );

 if action.orders.is_empty() {
 continue;
 }

 let nonce = std::time::SystemTime::now()
 .duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;

 match signer_t.sign_l1_action(&action, nonce, is_mainnet).await {
 Ok(signature) => {
 let oids_before = feed_t.open_order_oids.lock().await.len();
 feed_t.clear_post_only_reject_flag().await;

 match rest_client.send_l1_action(&action, nonce, signature).await {
 Ok(body) => {
 if exchange_order_submission_ok(&body) {
 let new_oids = collect_resting_oids_from_exchange_response(&body);
 if !new_oids.is_empty() {
 feed_t.open_order_oids.lock().await.extend(new_oids);
 }
 info!(" ÉLES LÉTRA KILŐVE (HTTP)");
 } else {
 if exchange_response_has_post_only_reject(&body) {
 feed_t.set_post_only_reject_flag(true).await;
 }
 }
 }
 Err(e) => tracing::error!(" Létra HTTP hiba: {}", e),
 }

 // Retry logika egyszerűsítve a stabilitás érdekében...
 let mut rejected_post_only = false;
 for _ in 0..22 {
 tokio::time::sleep(tokio::time::Duration::from_millis(45)).await;
 if feed_t.post_only_reject_pending().await {
 rejected_post_only = true;
 let _ = feed_t.consume_post_only_reject_flag().await;
 break;
 }
 if feed_t.open_order_oids.lock().await.len() > oids_before {
 break;
 }
 }

 if rejected_post_only {
 let target_usd_retry = *target_notional_t.lock().await;
 for (attempt, buf_ticks) in [(1_u32, 28.0_f64), (2, 55.0_f64)] {
 let (bb, ba, mid_r) = {
 let s = state_t.read().await;
 let m = if s.best_bid > 0.0 && s.best_ask > 0.0 { (s.best_bid + s.best_ask) / 2.0 } else { signal.target_mid };
 (s.best_bid, s.best_ask, m)
 };
 let retry_action = order_manager.build_ladder_payload_with_passive_buffer(
 &signal.side, mid_r, bb, ba, target_usd_retry, buf_ticks, max_close_sz,
 );
 if retry_action.orders.is_empty() { break; }

 let retry_nonce = std::time::SystemTime::now()
 .duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;

 if let Ok(retry_sig) = signer_t.sign_l1_action(&retry_action, retry_nonce, is_mainnet).await {
 feed_t.clear_post_only_reject_flag().await;
 if let Ok(rb) = rest_client.send_l1_action(&retry_action, retry_nonce, retry_sig).await {
 if exchange_order_submission_ok(&rb) {
 let ro = collect_resting_oids_from_exchange_response(&rb);
 if !ro.is_empty() { feed_t.open_order_oids.lock().await.extend(ro); }
 info!(" POST-ONLY RETRY #{}", attempt);
 break;
 }
 if exchange_response_has_post_only_reject(&rb) && attempt == 1 { continue; }
 break;
 }
 }
 }
 }
 last_signal_time = std::time::Instant::now();
 }
 Err(e) => tracing::error!(" Hiba az order aláírásakor: {}", e),
 }
 }
 tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
 }
 });

 // 8. FAILSAFE reconcile loop
 let coin_reconcile = coin.clone();
 let min_tick_reconcile = app_config.strategy.min_tick_size;
 let tp_min_ticks_reconcile = app_config.strategy.tp_min_ticks;
 let sl_min_ticks_reconcile = app_config.strategy.sl_min_ticks;
 let maker_fee_rate_reconcile = app_config.strategy.maker_fee_rate;
 let taker_fee_rate_reconcile = app_config.strategy.taker_fee_rate;
 let is_mainnet_reconcile = is_mainnet;

 tokio::spawn(async move {
 let mut last_protected_pos: f64 = 0.0;
 loop {
 tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

 match rest_client_r.get_user_state(hl_user_r.as_str()).await {
 Ok(state) => {
 let mut exchange_pos = 0.0_f64;
 let mut entry_px = None::<f64>;

 if let Some(arr) = state["assetPositions"].as_array() {
 for ap in arr {
 let pos = &ap["position"];
 if pos["coin"].as_str().unwrap_or("") == coin_reconcile {
 exchange_pos = pos["szi"].as_str().unwrap_or("0")
 .parse::<f64>().unwrap_or(0.0);
 entry_px = pos["entryPx"].as_str()
 .and_then(|v| v.parse::<f64>().ok());
 break;
 }
 }
 }

 *pos_reconcile_t.lock().await = exchange_pos;

 if exchange_pos.abs() < 0.0001 {
 last_protected_pos = 0.0;
 continue;
 }

 let fe_prot = match rest_client_r.get_frontend_open_orders(hl_user_r.as_str()).await {
 Ok(v) => v,
 Err(_) => continue,
 };

 // ── 1. VESZÉLYES LÉTRA ORDEREK SZIGORÚ TÖRLÉSE ──
 let dangerous_oids: Vec<u64> = if let Some(arr) = fe_prot.as_array() {
 arr.iter()
 .filter(|o| o["coin"].as_str() == Some(coin_reconcile.as_str()))
 .filter(|o| {
 let is_tpsl = o["isPositionTpsl"].as_bool().unwrap_or(false);
 let is_reduce_only = o["reduceOnly"].as_bool().unwrap_or(false);
 // HA nem TP/SL trigger ÉS nem reduce only, azonnal kinyírjuk, mert pozíció mellett ez halálos.
 !is_tpsl && !is_reduce_only
 })
 .filter_map(|o| {
 o["oid"].as_u64().or_else(|| o["oid"].as_str().and_then(|v| v.parse().ok()))
 })
 .collect()
 } else {
 Vec::new()
 };

 if !dangerous_oids.is_empty() {
 tracing::warn!(" RECONCILE: {} db veszélyes létra order törlése!", dangerous_oids.len());
 let cancel_l = om_reconcile.build_cancel_payload(&dangerous_oids);
 let cl = std::time::SystemTime::now()
 .duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
 if let Ok(csig) = signer_r.sign_l1_action(&cancel_l, cl, is_mainnet_reconcile).await {
 let _ = rest_client_r.send_l1_action(&cancel_l, cl, csig).await;
 }
 }

 // ── 2. TP/SL VÉDELEM ──
 if (exchange_pos.abs() - last_protected_pos.abs()).abs() < 0.001 {
 continue;
 }

 if frontend_position_tpsl_matches_pos(&fe_prot, &coin_reconcile, exchange_pos.abs()) {
 last_protected_pos = exchange_pos;
 continue;
 }

 let stale_oids = collect_position_tpsl_oids(&fe_prot, &coin_reconcile);
 if !stale_oids.is_empty() {
 let cancel_a = om_reconcile.build_cancel_payload(&stale_oids);
 let cn = std::time::SystemTime::now()
 .duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
 if let Ok(csig) = signer_r.sign_l1_action(&cancel_a, cn, is_mainnet_reconcile).await {
 let _ = rest_client_r.send_l1_action(&cancel_a, cn, csig).await;
 }
 }

 let reference_px = entry_px.unwrap_or(0.0);
 let reference_px = if reference_px > 0.0 { reference_px } else { state_reconcile.read().await.mid_price };

 if reference_px <= 0.0 { continue; }

 let vol = *vol_reconcile_t.lock().await;
 let tp_side = if exchange_pos > 0.0 { "Sell" } else { "Buy" };
 let mark_mid = state_reconcile.read().await.mid_price;

 let (tp_price, sl_price) = match OrderManager::tp_sl_prices_for_position(
 exchange_pos, reference_px, vol, min_tick_reconcile, tp_min_ticks_reconcile,
 sl_min_ticks_reconcile, mark_mid, maker_fee_rate_reconcile, taker_fee_rate_reconcile,
 ) {
 Some(p) => p,
 None => continue,
 };

 let protection = om_reconcile.build_protective_tpsl_payload(tp_side, tp_price, sl_price, exchange_pos.abs());
 let nonce = std::time::SystemTime::now()
 .duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;

 if let Ok(sig) = signer_r.sign_l1_action(&protection, nonce, is_mainnet_reconcile).await {
 if let Ok(body) = rest_client_r.send_l1_action(&protection, nonce, sig).await {
 if exchange_order_submission_ok(&body) {
 info!(" FAILSAFE TP/SL RECONCILE: pos={:.4}", exchange_pos);
 last_protected_pos = exchange_pos;
 }
 }
 }
 }
 Err(_) => continue,
 }
 }
 });

 tokio::signal::ctrl_c().await?;
 info!(" Leállítás...");
 Ok(())
}
