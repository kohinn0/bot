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
use crate::network::client::HyperliquidClient;
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
    let rest_client = HyperliquidClient::new(is_mainnet);
    
    info!("🔑 Pénztárca cím: {}", signer.get_address());

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
    let (feed, cmd_rx) = HyperliquidFeed::new(&coin, signer.get_address(), is_mainnet);
    let feed = Arc::new(feed);
    let state_ref = feed.state.clone();
    feed.clone().start(cmd_rx).await;

    let initial_balance = 99.0;
    let max_daily_loss_usd = app_config.strategy.max_daily_loss_usd;
    let max_daily_trades = app_config.strategy.max_daily_trades;
    let account_value = Arc::new(tokio::sync::Mutex::new(initial_balance));
    let trade_count = Arc::new(AtomicU32::new(0));
    let current_position = Arc::new(tokio::sync::Mutex::new(0.0));
    let last_volatility = Arc::new(tokio::sync::Mutex::new(0.01)); // Kezdeti volatilitás becslés
    let pnl_tracker = Arc::new(tokio::sync::Mutex::new(PnlTracker::new("../logs/pnl_state.json")));
    
    // Százalék kiszámítása az induló tőkéből
    let calculated_usd = initial_balance * (app_config.strategy.balance_pct_per_trade / 100.0) * (app_config.strategy.leverage as f64);
    let target_usd = calculated_usd.min(app_config.strategy.base_sz_usd);
    
    info!("💰 Kereskedési méret (notional): ${:.2} per szint", target_usd);
    
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
    let acc_t = account_value.clone();
    let pos_t = current_position.clone();
    let vol_t = last_volatility.clone();
    let feed_f = feed.clone();
    let signer_f = signer.clone();
    let om_f = Arc::new(OrderManager::new(app_config.strategy.clone(), asset_idx, sz_decimals));
    let min_tick = app_config.strategy.min_tick_size;
    let is_mainnet_f = is_mainnet;

    tokio::spawn(async move {
        while let Ok(fill) = fill_rx.recv().await {
            info!("📈 PNL/POS UPDATE: {} fill @ {} (Sz: {})", fill.coin, fill.px, fill.sz);
            
            let mut acc = acc_t.lock().await;
            let mut pnl = pnl_t.lock().await;
            let mut pos = pos_t.lock().await;
            
            let fill_sz = if fill.side == "B" { fill.sz } else { -fill.sz };
            *pos += fill_sz;

            let side_mult = if fill.side == "B" { -1.0 } else { 1.0 };
            *acc += fill.px * fill.sz * side_mult; 
            
            pnl.add_trade(0.0, fill.fee, *acc);
            trades_t.fetch_add(1, Ordering::Relaxed);
            info!("📊 Aktuális Pozíció: {:.4} {}", *pos, fill.coin);

            // 💡 MENTOR TRÜKK: Azonnali TP és SL elhelyezése kitöltés után
            // A volatilitást (std_dev) használjuk a szintek belövéséhez
            if *pos != 0.0 {
                let vol = { *vol_t.lock().await };
                let tp_side = if *pos > 0.0 { "Sell" } else { "Buy" };
                
                // Dinamikus TP: 1.5x szórás, de minimum 5 tick
                let tp_dist = f64::max(vol * 1.5, 5.0 * min_tick); 
                let tp_price = if *pos > 0.0 { fill.px + tp_dist } else { fill.px - tp_dist };

                // Dinamikus SL: 3x szórás, de minimum 10 tick
                let sl_dist = f64::max(vol * 3.0, 10.0 * min_tick);
                let sl_price = if *pos > 0.0 { fill.px - sl_dist } else { fill.px + sl_dist };

                // Use latest fill size for protective order sizing to avoid oversizing
                // when multiple partial fills arrive quickly.
                let protective_sz = fill.sz.abs();
                let exit_action = om_f.build_protective_tpsl_payload(tp_side, tp_price, sl_price, protective_sz);
                let e_nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;

                if let Ok(sig) = signer_f.sign_l1_action(&exit_action, e_nonce, is_mainnet_f).await {
                    let mut s_b = [0u8; 32]; sig.s.to_big_endian(&mut s_b);
                    let mut r_b = [0u8; 32]; sig.r.to_big_endian(&mut r_b);
                    let v = if sig.v < 27 { (sig.v + 27) as u8 } else { sig.v as u8 };
                    
                    let action_obj = exit_action; 
                    feed_f.send_action(serde_json::json!({
                        "action": action_obj,
                        "nonce": e_nonce,
                        "signature": {"r": format!("0x{}", hex::encode(r_b)), "s": format!("0x{}", hex::encode(s_b)), "v": v}
                    }));
                    info!("🛡️ EXCHANGE TP/SL KIHELYEZVE: {} TP @ {:.2} | SL @ {:.2}", tp_side, tp_price, sl_price);
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
    let feed_r = feed.clone();
    let rest_client_r = rest_client.clone();
    let pnl_sim_t = pnl_tracker.clone();
    let acc_sim_t = account_value.clone();
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
    let account_guard_t = account_value.clone();
    let trade_guard_t = trade_count.clone();

    // 6. A fő "szívverés" (Heartbeat) - Extrém gyors polling az RwLock-ból
    tokio::spawn(async move {
        loop {
            if let Some(signal) = signal_engine.tick().await {
                // Hard risk stop: halt new entries after daily loss/trade caps.
                let current_acc = *account_guard_t.lock().await;
                let drawdown = initial_balance - current_acc;
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

                // Pozíció és Piaci állapot lekérése
                let current_pos = { *pos_sim_t.lock().await };
                let (best_bid, best_ask) = {
                    let s = state_t.read().await;
                    (s.best_bid, s.best_ask)
                };

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
                    // --- 1. CLEAN SLATE: ELŐZŐ MEGBÍZÁSOK TÖRLÉSE ---
                    let mut cancel_oids = {
                        let mut tracked = feed_t.open_order_oids.lock().await;
                        let oids = tracked.clone();
                        tracked.clear();
                        oids
                    };

                    // Fallback: ha WS OID track üres, kérdezzük le REST-ről a nyitott ordereket.
                    if cancel_oids.is_empty() {
                        if let Ok(open_orders) = rest_client.get_open_orders(signer_t.get_address()).await {
                            if let Some(arr) = open_orders.as_array() {
                                let mut rest_found = 0usize;
                                for ord in arr {
                                    let coin_match = ord["coin"].as_str().map(|c| c == coin_signal).unwrap_or(false);
                                    let asset_match = ord["asset"]
                                        .as_u64()
                                        .map(|a| a == asset_idx as u64)
                                        .unwrap_or(false);
                                    if !(coin_match || asset_match) {
                                        continue;
                                    }
                                    let oid = ord["oid"]
                                        .as_u64()
                                        .or_else(|| ord["oid"].as_str().and_then(|v| v.parse::<u64>().ok()));
                                    if let Some(oid) = oid {
                                        cancel_oids.push(oid);
                                        rest_found += 1;
                                    }
                                }
                                if rest_found > 0 {
                                    info!("🛰️ REST openOrders fallback: {} db OID betöltve törléshez", rest_found);
                                }
                            }
                        }
                    }

                    if !cancel_oids.is_empty() {
                        let cancel_action = order_manager.build_cancel_payload(&cancel_oids);
                        let c_nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                        if let Ok(sig) = signer_t.sign_l1_action(&cancel_action, c_nonce, is_mainnet).await {
                            let mut s_b = [0u8; 32]; sig.s.to_big_endian(&mut s_b);
                            let mut r_b = [0u8; 32]; sig.r.to_big_endian(&mut r_b);
                            let v = if sig.v < 27 { (sig.v + 27) as u8 } else { sig.v as u8 };

                            feed_t.send_action(serde_json::json!({
                                "action": cancel_action,
                                "nonce": c_nonce,
                                "signature": {"r": format!("0x{}", hex::encode(r_b)), "s": format!("0x{}", hex::encode(s_b)), "v": v}
                            }));
                            info!("🧹 SZELLEM-ORDERS TÖRÖLVE ({} db Cancel)", cancel_oids.len());
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
                    let action = order_manager.build_ladder_payload(&signal.side, signal.target_mid, best_bid, best_ask, target_usd);
                    if action.orders.is_empty() {
                        tracing::warn!("⚠️ Üres order lista, létra küldés kihagyva (szűrők minden szintet eldobtak).");
                        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                        continue;
                    }
                    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;

                    match signer_t.sign_l1_action(&action, nonce, is_mainnet).await {
                        Ok(signature) => {
                            let mut s_bytes = [0u8; 32]; signature.s.to_big_endian(&mut s_bytes);
                            let mut r_bytes = [0u8; 32]; signature.r.to_big_endian(&mut r_bytes);
                            let v = if signature.v < 27 { (signature.v + 27) as u8 } else { signature.v as u8 };

                            let payload = serde_json::json!({
                                "action": action,
                                "nonce": nonce,
                                "signature": {"r": format!("0x{}", hex::encode(r_bytes)), "s": format!("0x{}", hex::encode(s_bytes)), "v": v}
                            });

                            feed_t.send_action(payload);
                            info!("🚀 ÉLES LÉTRA KILŐVE (Dinamikus árazás)");
                            last_signal_time = std::time::Instant::now();
                        },
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

            match rest_client_r.get_user_state(signer_r.get_address()).await {
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
                            let tp_price = if exchange_pos > 0.0 { reference_px + tp_dist } else { reference_px - tp_dist };
                            let sl_price = if exchange_pos > 0.0 { reference_px - sl_dist } else { reference_px + sl_dist };

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

                            if let Ok(sig) = signer_r.sign_l1_action(&protection, nonce, is_mainnet_reconcile).await {
                                let mut s_b = [0u8; 32];
                                sig.s.to_big_endian(&mut s_b);
                                let mut r_b = [0u8; 32];
                                sig.r.to_big_endian(&mut r_b);
                                let v = if sig.v < 27 { (sig.v + 27) as u8 } else { sig.v as u8 };

                                feed_r.send_action(serde_json::json!({
                                    "action": protection,
                                    "nonce": nonce,
                                    "signature": {"r": format!("0x{}", hex::encode(r_b)), "s": format!("0x{}", hex::encode(s_b)), "v": v}
                                }));
                                info!(
                                    "🛡️ FAILSAFE TP/SL RECONCILE: pos={:.4}, TP={:.4}, SL={:.4}",
                                    exchange_pos, tp_price, sl_price
                                );
                                last_protected_pos = exchange_pos;
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
