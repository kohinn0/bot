mod config;
mod network;
mod logic;

use dotenvy::dotenv;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;
use std::sync::Arc;

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

    use crate::config::AppConfig;
    use crate::logic::signer::HyperliquidSigner;
    use crate::network::client::HyperliquidClient;
    use crate::network::feed::HyperliquidFeed;

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
    let (feed, cmd_rx) = HyperliquidFeed::new(&coin, signer.get_address());
    let feed = Arc::new(feed);
    let state_ref = feed.state.clone();
    feed.clone().start(cmd_rx).await;

    let initial_balance = 99.0;
    let account_value = Arc::new(tokio::sync::Mutex::new(initial_balance));
    let pnl_tracker = Arc::new(tokio::sync::Mutex::new(PnlTracker::new("../logs/pnl_state.json")));
    
    // Százalék kiszámítása az induló tőkéből
    let calculated_usd = initial_balance * (app_config.strategy.balance_pct_per_trade / 100.0) * (app_config.strategy.leverage as f64);
    let target_usd = calculated_usd.min(app_config.strategy.base_sz_usd);
    
    info!("💰 Kereskedési méret (notional): ${:.2} per szint", target_usd);
    
    // 5. Szignál motor és Order Manager inicializálása
    let mut signal_engine = SignalEngine::new(app_config.strategy.clone(), state_ref.clone());
    let order_manager = OrderManager::new(app_config.strategy.clone(), asset_idx, sz_decimals);
    
    let is_dry_run = app_config.is_dry_run;

    // Aláíró és Kliens felkészítése a Hálózathoz
    let signer = Arc::new(signer);
    let rest_client = Arc::new(rest_client);

    // === VALÓS IDEJŰ FILL FIGYELŐ (PNL FRISSÍTÉS) ===
    let mut fill_rx = feed.fill_tx.subscribe();
    let pnl_t = pnl_tracker.clone();
    let acc_t = account_value.clone();
    
    tokio::spawn(async move {
        while let Ok(fill) = fill_rx.recv().await {
            info!("📈 PNL UPDATE: {} fill @ {} (Sz: {})", fill.coin, fill.px, fill.sz);
            
            let mut acc = acc_t.lock().await;
            let mut pnl = pnl_t.lock().await;
            
            // Élesben a fill-ek alapján frissítjük az egyenleget (leegyszerűsítve)
            // Megjegyzés: Ez egy market maker bot, ahol a Maker fill-ek csökkentik/növelik az inventory-t
            let side_mult = if fill.side == "B" { -1.0 } else { 1.0 };
            *acc += fill.px * fill.sz * side_mult; // Nagyon leegyszerűsített PnL modell
            
            pnl.add_trade(0.0, fill.fee, *acc);
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
    let pnl_sim_t = pnl_tracker.clone();
    let acc_sim_t = account_value.clone();

    // 6. A fő "szívverés" (Heartbeat) - Extrém gyors polling az RwLock-ból
    tokio::spawn(async move {
        loop {
            if let Some(signal) = signal_engine.tick().await {
                info!("🚨 SZIGNÁL: {} @ {:.4}", signal.side, signal.target_mid);
                
                let action = order_manager.build_ladder_payload(&signal.side, signal.target_mid, target_usd);
                
                let nonce = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64;

                if is_dry_run {
                    info!("🧪 DRY RUN: Szimulált megbízás elkészítve.");
                    let mut acc = acc_sim_t.lock().await;
                    let mut pnl = pnl_sim_t.lock().await;
                    *acc += 2.50; 
                    pnl.add_trade(2.55, 0.05, *acc);
                } else {
                    match signer_t.sign_l1_action(&action, nonce, is_mainnet).await {
                        Ok(signature) => {
                            // --- LOW LATENCY WS ORDER SUBMISSION ---
                            // Itt már nem várunk a HTTP válaszra, csak "kilőjük" a megbízást a WS-en
                            let mut s_bytes = [0u8; 32];
                            signature.s.to_big_endian(&mut s_bytes);
                            let mut r_bytes = [0u8; 32];
                            signature.r.to_big_endian(&mut r_bytes);
                            
                            let v_val = signature.v as u8;
                            let v = if v_val < 27 { v_val + 27 } else { v_val };

                            let payload = serde_json::json!({
                                "action": action,
                                "nonce": nonce,
                                "signature": {
                                    "r": format!("0x{}", hex::encode(r_bytes)),
                                    "s": format!("0x{}", hex::encode(s_bytes)),
                                    "v": v
                                }
                            });

                            feed_t.send_action(payload);
                            info!("🚀 ÉLES MEGBÍZÁS KILŐVE (WS) - Latency estim: <50ms");
                        },
                        Err(e) => tracing::error!("❌ Hiba az order aláírásakor: {}", e),
                    }
                }

                tokio::time::sleep(tokio::time::Duration::from_millis(5000)).await;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
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
