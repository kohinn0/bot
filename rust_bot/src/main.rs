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
    frontend_position_tpsl_matches_pos,
    collect_position_tpsl_oids,
    HyperliquidClient,
};
use crate::network::feed::HyperliquidFeed;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder().with_max_level(Level::INFO).finish();
    tracing::subscriber::set_global_default(subscriber).expect("Failed to set tracing subscriber");
    dotenv().ok();
    info!("🚀 INICIALIZÁLÁS: SebessegBot v3 (Rust/Tokio) 🚀");

    let app_config = AppConfig::load();
    let coin = app_config.strategy.coin.clone();
    let is_mainnet = app_config.is_mainnet;
    let signer = HyperliquidSigner::new(&app_config.private_key);
    let rest_client = HyperliquidClient::new(is_mainnet, app_config.hl_perp_dex.clone());
    let signer_addr = signer.get_address().to_string();
    let hl_user = app_config.hl_user_address.as_ref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| signer_addr.clone());

    let meta = rest_client.get_meta().await.expect("❌ Nem sikerült lekérni a meta adatokat");
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

    let feed = Arc::new(HyperliquidFeed::new(&coin, &hl_user, is_mainnet));
    let state_ref = feed.state.clone();
    feed.clone().start().await;

    let use_hl_equity = app_config.use_wallet_balance_for_sizing && !app_config.is_dry_run;
    let (wallet_equity_usd, _) = if use_hl_equity {
        match rest_client.get_account_value_usd(hl_user.as_str()).await {
            Ok(v) if v.is_finite() && v > 0.0 => (v, "API"),
            _ => (app_config.starting_equity_usd, "fallback"),
        }
    } else {
        (app_config.starting_equity_usd, "fallback")
    };

    let session_start_equity = wallet_equity_usd;
    let wallet_equity = Arc::new(tokio::sync::Mutex::new(wallet_equity_usd));
    let initial_notional = app_config.strategy.notional_per_level_usd(wallet_equity_usd);
    let target_notional_usd = Arc::new(tokio::sync::Mutex::new(initial_notional));
    let trade_count = Arc::new(AtomicU32::new(0));
    let current_position = Arc::new(tokio::sync::Mutex::new(0.0f64));
    let last_volatility = Arc::new(tokio::sync::Mutex::new(0.01f64));

    if use_hl_equity {
        let rest_w = rest_client.clone();
        let addr_w = hl_user.clone();
        let wallet_w = wallet_equity.clone();
        let target_w = target_notional_usd.clone();
        let strat_w = app_config.strategy.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                if let Ok(v) = rest_w.get_account_value_usd(&addr_w).await {
                    if v > 0.0 {
                        *wallet_w.lock().await = v;
                        *target_w.lock().await = strat_w.notional_per_level_usd(v);
                    }
                }
            }
        });
    }

    let mut signal_engine = SignalEngine::new(app_config.strategy.clone(), state_ref.clone());
    let mut order_manager = OrderManager::new(app_config.strategy.clone(), asset_idx, sz_decimals);
    let is_dry_run = app_config.is_dry_run;
    let signer = Arc::new(signer);
    let rest_client = Arc::new(rest_client);

    let mut fill_rx = feed.fill_tx.subscribe();
    let trades_t = trade_count.clone();
    tokio::spawn(async move {
        while let Ok(_) = fill_rx.recv().await {
            trades_t.fetch_add(1, Ordering::Relaxed);
        }
    });

    if !is_dry_run {
        let leverage_action = order_manager.build_leverage_payload();
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        if let Ok(sig) = signer.sign_l1_action(&leverage_action, nonce, is_mainnet).await {
            let _ = rest_client.send_l1_action(&leverage_action, nonce, sig).await;
        }
    }

    let signer_t = signer.clone();
    let feed_t = feed.clone();
    let rest_client_t = rest_client.clone();
    let pos_sim_t = current_position.clone();
    let vol_sim_t = last_volatility.clone();
    let state_t = state_ref.clone();
    let hl_user_t = hl_user.clone();
    
    // 🛡️ JAVÍTVA: A hiányzó memóriahivatkozások a belső aszinkron loophoz
    let target_notional_t = target_notional_usd.clone(); 
    let wallet_equity_t = wallet_equity.clone();

    let mut last_signal_time = std::time::Instant::now() - std::time::Duration::from_secs(60);
    let min_signal_interval = std::time::Duration::from_millis(app_config.strategy.min_signal_interval_ms);
    let max_pos_limit = app_config.strategy.max_positions;
    let coin_signal = coin.clone();

    tokio::spawn(async move {
        loop {
            if let Some(signal) = signal_engine.tick().await {
                let current_pos: f64 = *pos_sim_t.lock().await;
                let is_reducing = (current_pos > 0.001 && signal.side == "Sell") || (current_pos < -0.001 && signal.side == "Buy");

                if !is_reducing {
                    let current_eq = *wallet_equity_t.lock().await;
                    let drawdown = (session_start_equity - current_eq).max(0.0);
                    if drawdown >= (session_start_equity * 0.10) {
                        tracing::error!("🛑 MAX DRAWDOWN ELÉRVE. Új belépés letiltva!");
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                        continue;
                    }
                }

                if last_signal_time.elapsed() < min_signal_interval {
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                    continue;
                }

                *vol_sim_t.lock().await = signal.volatility;

                if max_pos_limit == 1 && current_pos.abs() > 0.001 && !is_reducing {
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    continue;
                }

                info!("🚨 SZIGNÁL: {} @ {:.4} | z={:.2}", signal.side, signal.target_mid, signal.z_score);
                order_manager.current_pos = current_pos;

                let max_close_sz = if is_reducing { Some(order_manager.quantize_position_sz(current_pos.abs())) } else { None };

                if is_dry_run { *pos_sim_t.lock().await += if signal.side == "Buy" { 0.1 } else { -0.1 }; continue; }

                let fe_orders = rest_client_t.get_frontend_open_orders(hl_user_t.as_str()).await.ok();
                let mut cancel_oids = { let mut t = feed_t.open_order_oids.lock().await; let o = t.clone(); t.clear(); o };
                
                if cancel_oids.is_empty() {
                    if let Some(ref fe) = fe_orders {
                        cancel_oids = collect_ladder_cancel_oids_from_frontend(fe, &coin_signal);
                    }
                }

                if let Some(ref fe) = fe_orders {
                    cancel_oids = filter_cancel_oids_excluding_position_tpsl_triggers(fe, &coin_signal, cancel_oids);
                }

                if !cancel_oids.is_empty() {
                    let cancel_action = order_manager.build_cancel_payload(&cancel_oids);
                    let c_nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                    if let Ok(sig) = signer_t.sign_l1_action(&cancel_action, c_nonce, is_mainnet).await {
                        let _ = rest_client_t.send_l1_action(&cancel_action, c_nonce, sig).await;
                    }
                }

                tokio::time::sleep(tokio::time::Duration::from_millis(15)).await;

                if !feed_t.open_order_oids.lock().await.is_empty() { continue; }

                let (best_bid, best_ask, ladder_mid) = {
                    let s = state_t.read().await;
                    (s.best_bid, s.best_ask, if s.best_bid > 0.0 && s.best_ask > 0.0 { (s.best_bid + s.best_ask) / 2.0 } else { signal.target_mid })
                };

                if best_bid <= 0.0 || best_ask <= 0.0 { continue; }

                let final_pos_check = *pos_sim_t.lock().await;
                if max_pos_limit == 1 && final_pos_check.abs() > 0.001 && !is_reducing { continue; }

                let target_usd = *target_notional_t.lock().await;
                let action = order_manager.build_ladder_payload(&signal.side, ladder_mid, best_bid, best_ask, target_usd, max_close_sz);
                if action.orders.is_empty() { continue; }

                let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                if let Ok(sig) = signer_t.sign_l1_action(&action, nonce, is_mainnet).await {
                    feed_t.clear_post_only_reject_flag().await;
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

    let signer_r = signer.clone();
    let hl_user_r = hl_user.clone();
    let rest_client_r = rest_client.clone();
    let pos_reconcile_t = current_position.clone();
    let state_reconcile = state_ref.clone();
    let vol_reconcile_t = last_volatility.clone();
    let om_reconcile = Arc::new(OrderManager::new(app_config.strategy.clone(), asset_idx, sz_decimals));
    let coin_reconcile = coin.clone();
    
    tokio::spawn(async move {
        let mut last_protected_pos: f64 = 0.0;
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            if let Ok(state) = rest_client_r.get_user_state(hl_user_r.as_str()).await {
                let mut exchange_pos = 0.0_f64;
                let mut entry_px = None::<f64>;
                if let Some(arr) = state["assetPositions"].as_array() {
                    for ap in arr {
                        let pos = &ap["position"];
                        if pos["coin"].as_str().unwrap_or("") == coin_reconcile {
                            exchange_pos = pos["szi"].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0);
                            entry_px = pos["entryPx"].as_str().and_then(|v| v.parse::<f64>().ok());
                            break;
                        }
                    }
                }
                *pos_reconcile_t.lock().await = exchange_pos;
                if exchange_pos.abs() < 0.0001 { last_protected_pos = 0.0; continue; }
                
                let fe_prot = match rest_client_r.get_frontend_open_orders(hl_user_r.as_str()).await { Ok(v) => v, Err(_) => continue };
                
                let dangerous_oids: Vec<u64> = if let Some(arr) = fe_prot.as_array() {
                    arr.iter().filter(|o| o["coin"].as_str() == Some(coin_reconcile.as_str()))
                       .filter(|o| !o["isPositionTpsl"].as_bool().unwrap_or(false) && !o["reduceOnly"].as_bool().unwrap_or(false))
                       .filter_map(|o| o["oid"].as_u64().or_else(|| o["oid"].as_str().and_then(|v| v.parse().ok()))).collect()
                } else { Vec::new() };

                if !dangerous_oids.is_empty() {
                    let cl = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                    let cancel_l = om_reconcile.build_cancel_payload(&dangerous_oids);
                    if let Ok(csig) = signer_r.sign_l1_action(&cancel_l, cl, is_mainnet).await { let _ = rest_client_r.send_l1_action(&cancel_l, cl, csig).await; }
                }

                if (exchange_pos.abs() - last_protected_pos.abs()).abs() < 0.001 { continue; }
                if frontend_position_tpsl_matches_pos(&fe_prot, &coin_reconcile, exchange_pos.abs()) { last_protected_pos = exchange_pos; continue; }

                let stale_oids = collect_position_tpsl_oids(&fe_prot, &coin_reconcile);
                if !stale_oids.is_empty() {
                    let cn = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                    let cancel_a = om_reconcile.build_cancel_payload(&stale_oids);
                    if let Ok(csig) = signer_r.sign_l1_action(&cancel_a, cn, is_mainnet).await { let _ = rest_client_r.send_l1_action(&cancel_a, cn, csig).await; }
                }

                let ref_px = if entry_px.unwrap_or(0.0) > 0.0 { entry_px.unwrap_or(0.0) } else { state_reconcile.read().await.mid_price };
                if ref_px <= 0.0 { continue; }

                if let Some((tp_price, sl_price)) = OrderManager::tp_sl_prices_for_position(
                    exchange_pos, ref_px, *vol_reconcile_t.lock().await, app_config.strategy.min_tick_size,
                    app_config.strategy.tp_min_ticks, app_config.strategy.sl_min_ticks, state_reconcile.read().await.mid_price,
                    app_config.strategy.maker_fee_rate, app_config.strategy.taker_fee_rate
                ) {
                    let protection = om_reconcile.build_protective_tpsl_payload(if exchange_pos > 0.0 { "Sell" } else { "Buy" }, tp_price, sl_price, exchange_pos.abs());
                    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                    if let Ok(sig) = signer_r.sign_l1_action(&protection, nonce, is_mainnet).await {
                        if let Ok(body) = rest_client_r.send_l1_action(&protection, nonce, sig).await {
                            if exchange_order_submission_ok(&body) { last_protected_pos = exchange_pos; }
                        }
                    }
                }
            }
        }
    });

    tokio::signal::ctrl_c().await?;
    info!("🛑 Leállítás...");
    Ok(())
}
