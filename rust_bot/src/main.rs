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
use crate::logic::bot_pnl::PnlTracker;
use crate::logic::signal::SignalEngine;
use crate::logic::order_manager::OrderManager;
use crate::network::client::{
    collect_ladder_cancel_oids_from_frontend, collect_ladder_cancel_oids_from_open_orders,
    cancel_response_only_benign_errors, collect_resting_oids_from_exchange_response,
    exchange_order_submission_ok, exchange_response_has_post_only_reject,
    filter_cancel_oids_excluding_position_tpsl_triggers,
    frontend_position_tpsl_matches_pos,
    HyperliquidClient,
};
use crate::network::feed::HyperliquidFeed;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Inicializáljuk a loggolást
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("Failed to set tracing subscriber");

    // Betöltjük a .env fájlt
    dotenv().ok();
    
    info!("🚀 INICIALIZÁLÁS: SebessegBot v3 (Rust/Tokio) 🚀");

    // 1. Konfiguráció Betöltése
    let app_config = AppConfig::load();
    let coin = app_config.strategy.coin.clone();

    // 2. Aláíró és Kliens inicializálása
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

    info!("🔐 Aláíró cím (PRIVATE_KEY): {}", signer_addr);
    if !hl_user.eq_ignore_ascii_case(&signer_addr) {
        info!(
            "👤 HL user (egyenleg, open orders, user WS): {}  ← HL_USER_ADDRESS",
            hl_user
        );
    } else {
        info!("👤 HL user = aláíró cím (HL_USER_ADDRESS nincs beállítva)");
    }

    // 3. Asset Meta lekérdezés a HL API-ból (keressük a coin Asset ID-ját és szDecimals-t)
    let meta = rest_client.get_meta().await.expect("❌ Nem sikerült lekérni a meta adatokat");
    let mut asset_idx = 0;
    let mut sz_decimals = 0;
    if let Some(universe) = meta["universe"].as_array() {
        for (idx, coin_data) in universe.iter().enumerate() {
            if coin_data["name"].as_str().unwrap_or("") == coin {
                asset_idx = idx as u32;
                sz_decimals = coin_data["szDecimals"].as_u64().unwrap_or(0) as u32;
                break;
            }
        }
    }
    info!("✅ Kereskedési pár: {}, Asset ID: {}, Size Decimals: {}", coin, asset_idx, sz_decimals);

    // 4. WebSocket Feed elindítása
    let feed = Arc::new(HyperliquidFeed::new(&coin, &hl_user, is_mainnet));
    let state_ref = feed.state.clone();
    feed.clone().start().await;

    let user_address = hl_user.clone();
    let max_daily_loss_usd = app_config.strategy.max_daily_loss_usd;
    let max_daily_trades = app_config.strategy.max_daily_trades;

    // Méret: Hyperliquid perp `accountValue` (clearinghouseState), ha engedélyezett; különben STARTING_EQUITY_USD
    let use_hl_equity =
        app_config.use_wallet_balance_for_sizing && !app_config.is_dry_run;
    let (wallet_equity_usd, equity_source): (f64, String) = if use_hl_equity {
        match rest_client.get_account_value_usd(user_address.as_str()).await {
            Ok(v) if v.is_finite() && v > 0.0 => (v, "Hyperliquid perp + spot USDC (API)".to_string()),
            Ok(v) => {
                tracing::warn!(
                    "⚠️ HL API egyenleg ~0 (${:.4}) — fallback STARTING_EQUITY_USD. Agent kulcsnál állítsd a HL_USER_ADDRESS-t a Portfolio címre; builder DEX → HL_PERP_DEX; mainnet/testnet.",
                    v
                );
                (
                    app_config.starting_equity_usd,
                    "fallback STARTING_EQUITY_USD (érvénytelen HL)".to_string(),
                )
            }
            Err(e) => {
                tracing::warn!(
                    "⚠️ HL egyenleg lekérés sikertelen: {} — fallback STARTING_EQUITY_USD",
                    e
                );
                (
                    app_config.starting_equity_usd,
                    "fallback STARTING_EQUITY_USD (API hiba)".to_string(),
                )
            }
        }
    } else {
        let why = if app_config.is_dry_run {
            "DRY_RUN"
        } else if !app_config.use_wallet_balance_for_sizing {
            "USE_WALLET_BALANCE_FOR_SIZING=false"
        } else {
            "kikapcsolva"
        };
        (
            app_config.starting_equity_usd,
            format!("STARTING_EQUITY_USD ({})", why),
        )
    };

    // Session drawdown: induló HL/fallback egyenleg (bot indítás pillanata)
    let session_start_equity = wallet_equity_usd;
    info!(
        "💵 Számlaérték (méret + drawdown bázis): ${:.2} — forrás: {}",
        wallet_equity_usd, equity_source
    );

    let wallet_equity = Arc::new(tokio::sync::Mutex::new(wallet_equity_usd));
    let initial_notional = app_config
        .strategy
        .notional_per_level_usd(wallet_equity_usd);
    let target_notional_usd = Arc::new(tokio::sync::Mutex::new(initial_notional));

    let trade_count = Arc::new(AtomicU32::new(0));
    let current_position = Arc::new(tokio::sync::Mutex::new(0.0));
    let last_volatility = Arc::new(tokio::sync::Mutex::new(0.01)); // Kezdeti volatilitás becslés
    let pnl_tracker = Arc::new(tokio::sync::Mutex::new(PnlTracker::new("../logs/pnl_state.json")));

    info!(
        "💰 Kereskedési méret (notional): ${:.2} per szint (követi a számlaértéket, ha HL frissítés fut)",
        initial_notional
    );

    // Háttérben: számlaérték + cél-notional frissítése a tőzsdéről
    if use_hl_equity {
        let rest_w = rest_client.clone();
        let addr_w = user_address.clone(); // String — 'static a háttér taskban
        let wallet_w = wallet_equity.clone();
        let target_w = target_notional_usd.clone();
        let strat_w = app_config.strategy.clone();
        let sec = app_config.wallet_equity_refresh_sec.max(10);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(sec)).await;
                match rest_w.get_account_value_usd(&addr_w).await {
                    Ok(v) => {
                        if v.is_finite() && v > 0.0 {
                            let n = strat_w.notional_per_level_usd(v);
                            {
                                let mut w = wallet_w.lock().await;
                                *w = v;
                            }
                            {
                                let mut t = target_w.lock().await;
                                *t = n;
                            }
                            tracing::debug!(
                                "🔄 HL egyenleg frissítve: ${:.2} → notional/szint ${:.2}",
                                v,
                                n
                            );
                        } else {
                            tracing::warn!("⚠️ HL accountValue kihagyva (érvénytelen): {}", v);
                        }
                    }
                    Err(e) => tracing::warn!("⚠️ HL egyenleg frissítés hiba: {}", e),
                }
            }
        });
    }
    
    // 5. Szignál motor és Order Manager inicializálása
    let mut signal_engine = SignalEngine::new(app_config.strategy.clone(), state_ref.clone());
    let mut order_manager = OrderManager::new(app_config.strategy.clone(), asset_idx, sz_decimals);
    
    let is_dry_run = app_config.is_dry_run;

    // Aláíró és Kliens felkészítése a Hálózathoz
    let signer = Arc::new(signer);
    let rest_client = Arc::new(rest_client);
    // === VALÓS IDEJŰ FILL ÉS POZÍCIÓ FIGYELŐ ===
    let mut fill_rx = feed.fill_tx.subscribe();
    let pnl_t = pnl_tracker.clone();
    let trades_t = trade_count.clone();
    let acc_t = wallet_equity.clone();
    let pos_t = current_position.clone();
    let vol_t = last_volatility.clone();
    let state_for_fill = state_ref.clone();
    let signer_f = signer.clone();
    let rest_client_f = rest_client.clone();
    let om_f = Arc::new(OrderManager::new(app_config.strategy.clone(), asset_idx, sz_decimals));
    let min_tick = app_config.strategy.min_tick_size;
    let is_mainnet_f = is_mainnet;

    tokio::spawn(async move {
        while let Ok(fill) = fill_rx.recv().await {
            info!("📈 PNL/POS UPDATE: {} fill @ {} (Sz: {})", fill.coin, fill.px, fill.sz);
            
            let mut pnl = pnl_t.lock().await;
            let mut pos = pos_t.lock().await;
            
            let fill_sz = if fill.side == "B" { fill.sz } else { -fill.sz };
            *pos += fill_sz;

            let eq_for_pnl = *acc_t.lock().await;
            pnl.add_trade(0.0, fill.fee, eq_for_pnl);
            trades_t.fetch_add(1, Ordering::Relaxed);
            info!("📊 Aktuális Pozíció: {:.4} {}", *pos, fill.coin);

            // 💡 MENTOR TRÜKK: Azonnali TP és SL elhelyezése kitöltés után
            // A volatilitást (std_dev) használjuk a szintek belövéséhez
            if *pos != 0.0 {
                let vol = { *vol_t.lock().await };
                let tp_side = if *pos > 0.0 { "Sell" } else { "Buy" };
                
                // Dinamikus TP: 1.5x szórás, de minimum 5 tick
                let tp_dist = f64::max(vol * 1.5, 5.0 * min_tick); 
                let raw_tp = if *pos > 0.0 { fill.px + tp_dist } else { fill.px - tp_dist };

                // Dinamikus SL: 3x szórás, de minimum 10 tick
                let sl_dist = f64::max(vol * 3.0, 10.0 * min_tick);
                let raw_sl = if *pos > 0.0 { fill.px - sl_dist } else { fill.px + sl_dist };

                let mark_mid = { state_for_fill.read().await.mid_price };
                let (tp_price, sl_price) = match OrderManager::clamp_tpsl_prices_for_mark(
                    *pos, raw_tp, raw_sl, mark_mid, min_tick,
                ) {
                    Some(p) => p,
                    None => {
                        tracing::warn!(
                            "🛡️ TP/SL kihagyva (clamp): mark={:.4} pos={:.4} raw_tp={:.4} raw_sl={:.4}",
                            mark_mid, *pos, raw_tp, raw_sl
                        );
                        continue;
                    }
                };

                // Teljes nyitott pozíció méretére TP/SL (nem csak az utolsó fill chunk).
                let protective_sz = om_f.quantize_position_sz(*pos);
                let exit_action = om_f.build_protective_tpsl_payload(tp_side, tp_price, sl_price, protective_sz);
                let e_nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;

                if let Ok(sig) = signer_f.sign_l1_action(&exit_action, e_nonce, is_mainnet_f).await {
                    match rest_client_f
                        .send_l1_action(&exit_action, e_nonce, sig)
                        .await
                    {
                        Ok(body) => {
                            if exchange_order_submission_ok(&body) {
                                info!(
                                    "🛡️ EXCHANGE TP/SL (HTTP): {} TP @ {:.2} | SL @ {:.2}",
                                    tp_side, tp_price, sl_price
                                );
                            } else {
                                tracing::warn!(
                                    "🛡️ TP/SL HTTP válasz (nem ok): {:?}",
                                    body
                                );
                            }
                        }
                        Err(e) => tracing::error!("🛡️ TP/SL HTTP küldés hiba: {}", e),
                    }
                }
            }
        }
    });

    // === INICIALIZÁLÓ LEVERAGE BEÁLLÍTÁS ===
    if !is_dry_run {
        info!("🔧 Kezdeti Tőkeáttétel {}x beállítása...", app_config.strategy.leverage);
        let leverage_action = order_manager.build_leverage_payload();
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;

        match signer.sign_l1_action(&leverage_action, nonce, is_mainnet).await {
            Ok(signature) => {
                // A leverage beállítást hagyhatjuk HTTP-n, mert csak egyszer fut le az elején
                match rest_client.send_l1_action(&leverage_action, nonce, signature).await {
                    Ok(_) => info!("✅ Tőkeáttétel sikeresen beállítva a tőzsdén!"),
                    Err(e) => tracing::error!("❌ Hiba a tőkeáttétel beállításánál: {}", e),
                }
            },
            Err(e) => tracing::error!("❌ Hiba a tőkeáttétel aláírásánál: {}", e),
        }
    }

    info!("⚙️ Kereskedési ciklus elindítva...");

    let signer_t = signer.clone();
    let feed_t = feed.clone();
    let signer_r = signer.clone();
    let hl_user_t = hl_user.clone();
    let hl_user_r = hl_user.clone();
    let rest_client_r = rest_client.clone();
    let pnl_sim_t = pnl_tracker.clone();
    let acc_sim_t = wallet_equity.clone();
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

    // 6. A fő "szívverés" (Heartbeat) - Extrém gyors polling az RwLock-ból
    tokio::spawn(async move {
        loop {
            if let Some(signal) = signal_engine.tick().await {
                // Hard risk stop: halt new entries after daily loss/trade caps.
                let current_equity = *account_guard_t.lock().await;
                let drawdown = (session_start_equity - current_equity).max(0.0);
                let trades_done = trade_guard_t.load(Ordering::Relaxed);
                if drawdown >= max_daily_loss_usd {
                    tracing::warn!(
                        "🛑 DAILY LOSS LIMIT ELÉRVE (dd=${:.2} >= ${:.2}), új belépések tiltva.",
                        drawdown,
                        max_daily_loss_usd
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }
                if trades_done >= max_daily_trades {
                    tracing::warn!(
                        "🛑 DAILY TRADE LIMIT ELÉRVE ({} >= {}), új belépések tiltva.",
                        trades_done,
                        max_daily_trades
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    continue;
                }

                // Cooldown ellenőrzése: ne küldjünk 500ms-on belül újabb létrát
                if last_signal_time.elapsed() < min_signal_interval {
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                    continue;
                }

                // Volatilitás mentése a Fill Listener számára
                {
                    let mut v = vol_sim_t.lock().await;
                    *v = signal.volatility;
                }

                // Pozíció lekérése (BBO-t a megbízás előtt frissítjük — cancel után már más lehet)
                let current_pos = { *pos_sim_t.lock().await };

                // Egyszerűsített max_positions kontroll: ha 1 a limit, és van pozíciónk, 
                // csak akkor engedünk újabb jelet, ha az ellentétes irányú (zárás)
                if max_pos_limit == 1 && current_pos.abs() > 0.001 {
                    let is_reducing = (current_pos > 0.0 && signal.side == "Sell") || (current_pos < 0.0 && signal.side == "Buy");
                    if !is_reducing {
                        // Nem engedünk rá több pozíciót ugyanabba az irányba
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                        continue;
                    }
                }

                info!("🚨 SZIGNÁL: {} @ {:.4} (Pos: {:.4})", signal.side, signal.target_mid, current_pos);
                
                // Szinkronizáljuk a pozíciót az order managerrel a skew hatáshoz
                order_manager.current_pos = current_pos;

                if is_dry_run {
                    info!("🧪 DRY RUN: Szimulált megbízás elkészítve.");
                    let mut acc = acc_sim_t.lock().await;
                    let mut pnl = pnl_sim_t.lock().await;
                    let mut pos = pos_sim_t.lock().await;
                    
                    *acc += 2.50; 
                    let fill_sz = if signal.side == "Buy" { 0.1 } else { -0.1 }; // fiktív méret
                    *pos += fill_sz;
                    pnl.add_trade(2.55, 0.05, *acc);
                } else {
                    // --- 1. CLEAN SLATE: ELŐZŐ LÉTRA ORDEREK (TP/SL trigger NEM törlődik) ---
                    let addr = hl_user_t.as_str();
                    let fe_orders = rest_client.get_frontend_open_orders(addr).await.ok();
                    let open_orders_basic = if fe_orders.is_none() {
                        match rest_client.get_open_orders(addr).await {
                            Ok(v) => {
                                tracing::warn!(
                                    "🛰️ frontendOpenOrders nem elérhető → openOrders fallback OID-gyűjtéshez"
                                );
                                Some(v)
                            }
                            Err(e) => {
                                tracing::warn!("🛰️ openOrders fallback is sikertelen: {}", e);
                                None
                            }
                        }
                    } else {
                        None
                    };

                    let mut cancel_oids = {
                        let mut tracked = feed_t.open_order_oids.lock().await;
                        let oids = tracked.clone();
                        tracked.clear();
                        oids
                    };

                    // Fallback: WS OID üres → frontend listából csak nem-trigger / nem-positionTpsl
                    if cancel_oids.is_empty() {
                        if let Some(ref fe) = fe_orders {
                            cancel_oids = collect_ladder_cancel_oids_from_frontend(fe, &coin_signal);
                            if !cancel_oids.is_empty() {
                                info!(
                                    "🛰️ REST frontendOpenOrders fallback: {} db létra-OID törléshez (TP/SL kihagyva)",
                                    cancel_oids.len()
                                );
                            }
                        } else if let Some(ref oo) = open_orders_basic {
                            cancel_oids =
                                collect_ladder_cancel_oids_from_open_orders(oo, &coin_signal);
                            if !cancel_oids.is_empty() {
                                info!(
                                    "🛰️ REST openOrders fallback: {} db OID törléshez (csak egyszerű limit; frontend hiányzik)",
                                    cancel_oids.len()
                                );
                            }
                        }
                    }

                    if let Some(ref fe) = fe_orders {
                        let before = cancel_oids.len();
                        cancel_oids =
                            filter_cancel_oids_excluding_position_tpsl_triggers(fe, &coin_signal, cancel_oids);
                        if before != cancel_oids.len() {
                            info!(
                                "🛡️ TP/SL trigger OID-ek kihagyva a törlésből ({} → {} oid)",
                                before,
                                cancel_oids.len()
                            );
                        }
                    } else if open_orders_basic.is_some() && !cancel_oids.is_empty() {
                        tracing::warn!(
                            "🛡️ TP/SL OID szűrés kihagyva (nincs frontendOpenOrders); openOrders alapú lista lehet kevésbé biztos"
                        );
                    }

                    if !cancel_oids.is_empty() {
                        let cancel_action = order_manager.build_cancel_payload(&cancel_oids);
                        let c_nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                        if let Ok(sig) = signer_t.sign_l1_action(&cancel_action, c_nonce, is_mainnet).await {
                            match rest_client
                                .send_l1_action(&cancel_action, c_nonce, sig)
                                .await
                            {
                                Ok(body) => {
                                    if exchange_order_submission_ok(&body) {
                                        info!(
                                            "🧹 SZELLEM-ORDERS TÖRÖLVE ({} db Cancel, HTTP)",
                                            cancel_oids.len()
                                        );
                                    } else if cancel_response_only_benign_errors(&body) {
                                        info!(
                                            "🧹 Cancel: oid(ek) már nem élnek (kitöltve/törölve) — rendben."
                                        );
                                    } else {
                                        tracing::warn!("🧹 Cancel HTTP válasz: {:?}", body);
                                    }
                                }
                                Err(e) => tracing::error!("🧹 Cancel HTTP hiba: {}", e),
                            }
                        }
                    } else {
                        info!("🧹 Nincs törlendő nyitott order ezen a coinon.");
                    }
                    
                    // Adunk pár ms-t a tőzsdének, hogy feldolgozza a törlést
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

                    // Hard guard: ha van még lokálisan nyitottként követett order, ne küldjünk új létrát.
                    let has_open_orders = !feed_t.open_order_oids.lock().await.is_empty();
                    if has_open_orders {
                        info!("⛔ Új létra kihagyva: még vannak nyitott orderek {} piacon.", coin_signal);
                        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                        continue;
                    }

                    // --- 2. ÚJ LÉTRA KIHELYEZÉSE ---
                    let (best_bid, best_ask, ladder_mid) = {
                        let s = state_t.read().await;
                        let mid = if s.best_bid > 0.0 && s.best_ask > 0.0 {
                            (s.best_bid + s.best_ask) / 2.0
                        } else {
                            signal.target_mid
                        };
                        (s.best_bid, s.best_ask, mid)
                    };
                    let target_usd = *target_notional_t.lock().await;
                    let action = order_manager.build_ladder_payload(
                        &signal.side,
                        ladder_mid,
                        best_bid,
                        best_ask,
                        target_usd,
                    );
                    if action.orders.is_empty() {
                        tracing::warn!("⚠️ Üres order lista, létra küldés kihagyva (szűrők minden szintet eldobtak).");
                        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                        continue;
                    }
                    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;

                    match signer_t.sign_l1_action(&action, nonce, is_mainnet).await {
                        Ok(signature) => {
                            let oids_before = feed_t.open_order_oids.lock().await.len();
                            feed_t.clear_post_only_reject_flag().await;

                            match rest_client
                                .send_l1_action(&action, nonce, signature)
                                .await
                            {
                                Ok(body) => {
                                    if exchange_order_submission_ok(&body) {
                                        let new_oids =
                                            collect_resting_oids_from_exchange_response(&body);
                                        if !new_oids.is_empty() {
                                            let mut t = feed_t.open_order_oids.lock().await;
                                            t.extend(new_oids);
                                        }
                                        info!("🚀 ÉLES LÉTRA KILŐVE (HTTP)");
                                    } else {
                                        tracing::warn!("🚀 LÉTRA HTTP válasz (nem ok): {:?}", body);
                                        if exchange_response_has_post_only_reject(&body) {
                                            feed_t.set_post_only_reject_flag(true).await;
                                        }
                                    }
                                }
                                Err(e) => tracing::error!("🚀 Létra HTTP hiba: {}", e),
                            }

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
                                let (best_bid_r, best_ask_r, ladder_mid_r) = {
                                    let s = state_t.read().await;
                                    let mid = if s.best_bid > 0.0 && s.best_ask > 0.0 {
                                        (s.best_bid + s.best_ask) / 2.0
                                    } else {
                                        signal.target_mid
                                    };
                                    (s.best_bid, s.best_ask, mid)
                                };
                                let target_usd_retry = *target_notional_t.lock().await;
                                let retry_action = order_manager.build_ladder_payload_with_passive_buffer(
                                    &signal.side,
                                    ladder_mid_r,
                                    best_bid_r,
                                    best_ask_r,
                                    target_usd_retry,
                                    10.0,
                                );
                                if !retry_action.orders.is_empty() {
                                    let retry_nonce = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap()
                                        .as_millis() as u64;
                                    if let Ok(retry_sig) =
                                        signer_t.sign_l1_action(&retry_action, retry_nonce, is_mainnet).await
                                    {
                                        feed_t.clear_post_only_reject_flag().await;
                                        match rest_client
                                            .send_l1_action(&retry_action, retry_nonce, retry_sig)
                                            .await
                                        {
                                            Ok(rb) => {
                                                if exchange_order_submission_ok(&rb) {
                                                    let ro =
                                                        collect_resting_oids_from_exchange_response(
                                                            &rb,
                                                        );
                                                    if !ro.is_empty() {
                                                        let mut t =
                                                            feed_t.open_order_oids.lock().await;
                                                        t.extend(ro);
                                                    }
                                                    info!(
                                                        "🔁 POST-ONLY RETRY (HTTP), buffer=10 tick"
                                                    );
                                                } else {
                                                    tracing::warn!(
                                                        "🔁 Retry HTTP válasz: {:?}",
                                                        rb
                                                    );
                                                }
                                            }
                                            Err(e) => {
                                                tracing::error!("🔁 Retry HTTP hiba: {}", e)
                                            }
                                        }
                                    }
                                }
                            }
                            last_signal_time = std::time::Instant::now();
                        }
                        Err(e) => tracing::error!("❌ Hiba az order aláírásakor: {}", e),
                    }
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
        }
    });

    // === FAILSAFE: REST reconcile a pozícióra + védő TP/SL újraküldés ===
    let coin_reconcile = coin.clone();
    let min_tick_reconcile = min_tick;
    let is_mainnet_reconcile = is_mainnet;
    tokio::spawn(async move {
        let mut last_protected_pos: f64 = 0.0;
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

            match rest_client_r.get_user_state(hl_user_r.as_str()).await {
                Ok(state) => {
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

                    {
                        let mut p = pos_reconcile_t.lock().await;
                        *p = exchange_pos;
                    }

                    if exchange_pos.abs() < 0.0001 {
                        last_protected_pos = 0.0;
                        continue;
                    }

                    // Re-arm protection if position changed materially or wasn't protected yet.
                    if (exchange_pos.abs() - last_protected_pos.abs()).abs() >= 0.001 {
                        if let Ok(fe_prot) =
                            rest_client_r.get_frontend_open_orders(hl_user_r.as_str()).await
                        {
                            if frontend_position_tpsl_matches_pos(
                                &fe_prot,
                                &coin_reconcile,
                                exchange_pos.abs(),
                            ) {
                                last_protected_pos = exchange_pos;
                                tracing::debug!(
                                    "🛡️ FAILSAFE TP/SL kihagyva: már van position TP/SL a méretnek megfelelően"
                                );
                                continue;
                            }
                        }

                        let reference_px = if let Some(px) = entry_px {
                            px
                        } else {
                            let s = state_reconcile.read().await;
                            s.mid_price
                        };

                        if reference_px > 0.0 {
                            let vol = { *vol_reconcile_t.lock().await };
                            let tp_side = if exchange_pos > 0.0 { "Sell" } else { "Buy" };
                            let tp_dist = f64::max(vol * 1.5, 5.0 * min_tick_reconcile);
                            let sl_dist = f64::max(vol * 3.0, 10.0 * min_tick_reconcile);
                            let raw_tp = if exchange_pos > 0.0 {
                                reference_px + tp_dist
                            } else {
                                reference_px - tp_dist
                            };
                            let raw_sl = if exchange_pos > 0.0 {
                                reference_px - sl_dist
                            } else {
                                reference_px + sl_dist
                            };

                            let mark_mid = { state_reconcile.read().await.mid_price };
                            let (tp_price, sl_price) = match OrderManager::clamp_tpsl_prices_for_mark(
                                exchange_pos,
                                raw_tp,
                                raw_sl,
                                mark_mid,
                                min_tick_reconcile,
                            ) {
                                Some(p) => p,
                                None => {
                                    tracing::warn!(
                                        "🛡️ FAILSAFE TP/SL clamp skip: mark={:.4} pos={:.4} ref={:.4} raw_tp={:.4} raw_sl={:.4}",
                                        mark_mid, exchange_pos, reference_px, raw_tp, raw_sl
                                    );
                                    continue;
                                }
                            };

                            let protection = om_reconcile.build_protective_tpsl_payload(
                                tp_side,
                                tp_price,
                                sl_price,
                                exchange_pos.abs(),
                            );
                            let nonce = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_millis() as u64;

                            if let Ok(sig) =
                                signer_r.sign_l1_action(&protection, nonce, is_mainnet_reconcile).await
                            {
                                match rest_client_r
                                    .send_l1_action(&protection, nonce, sig)
                                    .await
                                {
                                    Ok(body) => {
                                        if exchange_order_submission_ok(&body) {
                                            info!(
                                                "🛡️ FAILSAFE TP/SL RECONCILE (HTTP): pos={:.4}, TP={:.4}, SL={:.4}",
                                                exchange_pos, tp_price, sl_price
                                            );
                                            last_protected_pos = exchange_pos;
                                        } else {
                                            tracing::warn!(
                                                "🛡️ FAILSAFE TP/SL HTTP válasz: {:?}",
                                                body
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!("🛡️ FAILSAFE TP/SL HTTP küldés: {}", e)
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("⚠️ REST reconcile hiba (user_state): {}", e);
                }
            }
        }
    });

    // Diagnosztikai kiírás
    let diag_state_ref = feed.state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            let current = diag_state_ref.read().await;
            info!(
                "📊 [{} L2Book] Mid: {:.4} | Bid: {:.4} | Ask: {:.4} | Imbalance: {:.2}",
                current.coin, current.mid_price, current.best_bid, current.best_ask, current.imbalance
            );
        }
    });

    tokio::signal::ctrl_c().await?;
    info!("🛑 Leállítás...");

    Ok(())
}
