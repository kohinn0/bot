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
    collect_ladder_cancel_oids_from_frontend,
    collect_resting_oids_from_exchange_response,
    exchange_order_submission_ok,
    filter_cancel_oids_excluding_position_tpsl_triggers,
    HyperliquidClient,
};
use crate::network::feed::HyperliquidFeed;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder().with_max_level(Level::INFO).finish();
    tracing::subscriber::set_global_default(subscriber).expect("Failed to set tracing subscriber");
    dotenv().ok();
    info!("🚀 INICIALIZÁLÁS: SebessegBot V4.4 (Dust & Stale Limit Killer) 🚀");

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

    let feed = Arc::new(HyperliquidFeed::new(&coin, &hl_user, is_mainnet));
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

    let mut signal_engine = SignalEngine::new(app_config.strategy.clone());
    let mut order_manager = OrderManager::new(app_config.strategy.clone(), asset_idx, sz_decimals);
    let (signer, rest_client) = (Arc::new(signer), Arc::new(rest_client));

    let (signer_t, feed_t, rest_client_t, pos_sim_t, vol_sim_t, state_t, hl_user_t, target_notional_t, wallet_equity_t) = 
        (signer.clone(), feed.clone(), rest_client.clone(), current_position.clone(), last_volatility.clone(), state_ref.clone(), hl_user.clone(), target_notional_usd.clone(), wallet_equity.clone());
    
    let mut last_signal_time = std::time::Instant::now() - std::time::Duration::from_secs(60);
    let min_signal_interval = std::time::Duration::from_millis(app_config.strategy.min_signal_interval_ms);
    let coin_signal = coin.clone();

    tokio::spawn(async move {
        loop {
            let mid = { let s = state_t.read().await; if s.best_bid > 0.0 && s.best_ask > 0.0 { (s.best_bid + s.best_ask) / 2.0 } else { 0.0 } };
                if let Some(signal) = signal_engine.tick(mid).await {
                let current_pos = *pos_sim_t.lock().await;
                let is_reducing = (current_pos.abs() > 0.001) && ((current_pos > 0.0 && signal.side == "Sell") || (current_pos < 0.0 && signal.side == "Buy"));

                if !is_reducing && (session_start_equity - *wallet_equity_t.lock().await) >= (session_start_equity * 0.10) { continue; }
                if last_signal_time.elapsed() < min_signal_interval { continue; }
                if app_config.strategy.max_positions == 1 && current_pos.abs() > 0.001 && !is_reducing { continue; }

                info!("🚨 SZIGNÁL: {} @ {:.4}", signal.side, signal.target_mid);
                *vol_sim_t.lock().await = signal.volatility;
                order_manager.current_pos = current_pos;

                let fe_orders = rest_client_t.get_frontend_open_orders(hl_user_t.as_str()).await.ok();
                let mut cancel_oids = { let mut t = feed_t.open_order_oids.lock().await; let o = t.clone(); t.clear(); o };
                if cancel_oids.is_empty() { if let Some(ref fe) = fe_orders { cancel_oids = collect_ladder_cancel_oids_from_frontend(fe, &coin_signal); } }
                if let Some(ref fe) = fe_orders { cancel_oids = filter_cancel_oids_excluding_position_tpsl_triggers(fe, &coin_signal, cancel_oids); }

                if !cancel_oids.is_empty() {
                    let c_action = order_manager.build_cancel_payload(&cancel_oids);
                    let c_nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                    if let Ok(sig) = signer_t.sign_l1_action(&c_action, c_nonce, app_config.is_mainnet).await { let _ = rest_client_t.send_l1_action(&c_action, c_nonce, sig).await; }
                }

                let s = state_t.read().await;
                let (bid, ask, mid) = (s.best_bid, s.best_ask, if s.best_bid > 0.0 { (s.best_bid + s.best_ask) / 2.0 } else { signal.target_mid });
                if bid <= 0.0 { continue; }

                let action = order_manager.build_ladder_payload(&signal.side, mid, bid, ask, *target_notional_t.lock().await, if is_reducing { Some(order_manager.quantize_position_sz(current_pos.abs())) } else { None });
                let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                if let Ok(sig) = signer_t.sign_l1_action(&action, nonce, app_config.is_mainnet).await {
                    if let Ok(body) = rest_client_t.send_l1_action(&action, nonce, sig).await {
                        if exchange_order_submission_ok(&body) {
                            let new_oids = collect_resting_oids_from_exchange_response(&body);
                            if !new_oids.is_empty() { feed_t.open_order_oids.lock().await.extend(new_oids); }
                        }
                    }
                    last_signal_time = std::time::Instant::now();
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
        }
    });

    let (signer_r, rest_client_r, pos_rec_t, state_rec, vol_rec_t, hl_user_r, coin_rec) = 
        (signer.clone(), rest_client.clone(), current_position.clone(), state_ref.clone(), last_volatility.clone(), hl_user.clone(), coin.clone());
    let om_rec = Arc::new(OrderManager::new(app_config.strategy.clone(), asset_idx, sz_decimals));

    tokio::spawn(async move {
        let mut last_protected_pos: f64 = 0.0;
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            if let Ok(st) = rest_client_r.get_user_state(hl_user_r.as_str()).await {
                let mut ex_pos = 0.0;
                let mut ent_px = None;
                if let Some(arr) = st["assetPositions"].as_array() {
                    for ap in arr {
                        if ap["position"]["coin"].as_str() == Some(&coin_rec) {
                            ex_pos = ap["position"]["szi"].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                            ent_px = ap["position"]["entryPx"].as_str().and_then(|v| v.parse().ok());
                            break;
                        }
                    }
                }
                *pos_rec_t.lock().await = ex_pos;
                let fe = match rest_client_r.get_frontend_open_orders(hl_user_r.as_str()).await { Ok(v) => v, Err(_) => continue };
                let ref_px = ent_px.unwrap_or(state_rec.read().await.mid_price);
                let notional_val = ex_pos.abs() * ref_px;

                // 🚨 1. DUST CLOSER: Ha a pozíció túl kicsi ($15 alatt), bezárja és töröl minden ordert
                if ex_pos.abs() > 0.0001 && notional_val < 15.0 {
                    info!("🗑️ Dust detektálva (${:.2}). Market Close indítása...", notional_val);
                    let close_action = om_rec.build_market_close_payload(ex_pos < 0.0, ref_px, ex_pos.abs());
                    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                    if let Ok(sig) = signer_r.sign_l1_action(&close_action, nonce, app_config.is_mainnet).await {
                        let _ = rest_client_r.send_l1_action(&close_action, nonce, sig).await;
                    }
                    
                    let all_oids: Vec<u64> = fe.as_array().unwrap_or(&vec![]).iter()
                        .filter(|o| o["coin"].as_str() == Some(&coin_rec))
                        .filter_map(|o| o["oid"].as_u64().or_else(|| o["oid"].as_str().and_then(|v| v.parse().ok()))).collect();
                    
                    if !all_oids.is_empty() {
                        let c_nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                        let cancel_a = om_rec.build_cancel_payload(&all_oids);
                        if let Ok(csig) = signer_r.sign_l1_action(&cancel_a, c_nonce, app_config.is_mainnet).await {
                            let _ = rest_client_r.send_l1_action(&cancel_a, c_nonce, csig).await;
                        }
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
                            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                            if let Ok(sig) = signer_r.sign_l1_action(&action, nonce, app_config.is_mainnet).await { let _ = rest_client_r.send_l1_action(&action, nonce, sig).await; }
                        }
                    }
                    last_protected_pos = 0.0; continue;
                }

                // 🛡️ 3. ANTI-STALE LADDER (Csak a védelem maradhat)
                if ex_pos.abs() > 0.0001 {
                    let stale: Vec<u64> = fe.as_array().unwrap_or(&vec![]).iter()
                        .filter(|o| o["coin"].as_str() == Some(&coin_rec) && !o["isPositionTpsl"].as_bool().unwrap_or(false) && !o["reduceOnly"].as_bool().unwrap_or(false))
                        .filter_map(|o| o["oid"].as_u64().or_else(|| o["oid"].as_str().and_then(|v| v.parse().ok()))).collect();
                    if !stale.is_empty() {
                        let action = om_rec.build_cancel_payload(&stale);
                        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                        if let Ok(sig) = signer_r.sign_l1_action(&action, nonce, app_config.is_mainnet).await { let _ = rest_client_r.send_l1_action(&action, nonce, sig).await; }
                    }

                    let has_tpsl = fe.as_array().unwrap_or(&vec![]).iter().any(|o| o["coin"].as_str() == Some(&coin_rec) && o["isPositionTpsl"].as_bool().unwrap_or(false));
                    if has_tpsl && (ex_pos.abs() - last_protected_pos.abs()).abs() < 0.0001 { continue; }
                }

                if (ex_pos.abs() - last_protected_pos.abs()).abs() < 0.001 { continue; }

                if ref_px <= 0.0 { continue; }
                if let Some((tp, sl)) = OrderManager::tp_sl_prices_for_position(ex_pos, ref_px, *vol_rec_t.lock().await, app_config.strategy.min_tick_size, app_config.strategy.tp_min_ticks, app_config.strategy.sl_min_ticks, state_rec.read().await.mid_price, app_config.strategy.maker_fee_rate, app_config.strategy.taker_fee_rate) {
                    let prot = om_rec.build_protective_tpsl_payload(if ex_pos > 0.0 { "Sell" } else { "Buy" }, tp, sl, ex_pos.abs());
                    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                    if let Ok(sig) = signer_r.sign_l1_action(&prot, nonce, app_config.is_mainnet).await {
                        if let Ok(_) = rest_client_r.send_l1_action(&prot, nonce, sig).await { last_protected_pos = ex_pos; }
                    }
                }
            }
        }
    });

    tokio::signal::ctrl_c().await?;
    Ok(())
}
