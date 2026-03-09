mod config;
mod network;
mod logic;

use dotenvy::dotenv;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

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
    let is_mainnet = true; // Todo: config.rs ből
    
    let signer = HyperliquidSigner::new(&app_config.private_key);
    let rest_client = HyperliquidClient::new(is_mainnet);
    
    info!("🔑 Pénztárca cím: {}", signer.get_address());

    // 3. Asset Meta lekérdezés a HL API-ból (keressük a coin Asset ID-ját)
    let meta = rest_client.get_meta().await.expect("❌ Nem sikerült lekérni a meta adatokat");
    let mut asset_idx = 0;
    if let Some(universe) = meta["universe"].as_array() {
        for (idx, coin_data) in universe.iter().enumerate() {
            if coin_data["name"].as_str().unwrap_or("") == coin {
                asset_idx = idx as u32;
                break;
            }
        }
    }
    info!("✅ Kereskedési pár: {}, Asset ID: {}", coin, asset_idx);

    // 4. WebSocket Feed elindítása (Külön Tokio szálon fog pörögni)
    let feed = HyperliquidFeed::new(&coin);
    let state_ref = feed.state.clone();
    feed.start().await;

    use crate::logic::signal::SignalEngine;
    use crate::logic::order_manager::OrderManager;
    use crate::logic::bot_pnl::PnlTracker;

    let mut simulated_balance_usd = 1000.0;
    
    // Százalék kiszámítása a 1000 dollárból, beállítva a maximum plafonnal (base_sz_usd)
    let calculated_usd = simulated_balance_usd * (app_config.strategy.balance_pct_per_trade / 100.0) * (app_config.strategy.leverage as f64);
    let target_usd = calculated_usd.min(app_config.strategy.base_sz_usd);
    
    info!("💰 Kereskedési méret beállítva: ${:.2} (Minden létra hossza)", target_usd);
    
    // 5. Szignál motor, Order Manager és PnL Tracker inicializálása
    let mut signal_engine = SignalEngine::new(app_config.strategy.clone(), state_ref.clone());
    let order_manager = OrderManager::new(app_config.strategy.clone(), asset_idx);
    let mut pnl_tracker = PnlTracker::new("../logs/pnl_state.json");
    
    info!("⚙️ Kereskedési ciklus elindítva...");

    // Aláíró és Kliens a Hálózathoz
    let signer = std::sync::Arc::new(signer);
    let rest_client = std::sync::Arc::new(rest_client);
    
    let signer_t = signer.clone();
    let rest_client_t = rest_client.clone();
    let is_dry_run = app_config.is_dry_run;

    // 5. A fő "szívverés" (Heartbeat) - Extrém gyors polling az RwLock-ból
    // Mivel aszinkron és lock-free a state_ref, ezt nyugodtan pörgethetjük 1ms delay-el
    tokio::spawn(async move {
        loop {
            if let Some(signal) = signal_engine.tick().await {
                info!("🚨 SZIGNÁL ÉSZLELVE: {} @ {:.4}", signal.side, signal.target_mid);
                
                let action = order_manager.build_ladder_payload(&signal.side, signal.target_mid, target_usd);
                
                let nonce = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64;

                info!("🛠️ Order Payload előkészítve: {}", serde_json::to_string(&action).unwrap());

                if is_dry_run {
                    // Mivel egyelőre éles bekötés még nincsen (csak Dry Run logolás),
                    // szimuláljuk, hogy bejött egy trade, és elmentjük a PnL fájlba a Dashboardnak.
                    simulated_balance_usd += 2.50; // Random nyereség szimuláció
                    pnl_tracker.add_trade(2.55, 0.05, simulated_balance_usd);
                } else {
                    info!(" ام Aláírás és küldés folyamatban (LIVE)...");
                    match signer_t.sign_l1_action(&action, nonce, is_mainnet).await {
                        Ok(signature) => {
                            info!("✅ Payload aláírva! EIP-712 Signature generálva.");
                            match rest_client_t.send_l1_action(&action, nonce, signature).await {
                                Ok(response) => info!("🎯 Order elküldve! Válasz: {}", response),
                                Err(e) => tracing::error!("❌ Hiba az order küldésekor: {}", e),
                            }
                        },
                        Err(e) => tracing::error!("❌ Hiba az order aláírásakor: {}", e),
                    }
                }

                // Cooldown: miután kiraktuk a letrát, várunk picit, hogy ne spammeljünk
                tokio::time::sleep(tokio::time::Duration::from_millis(5000)).await;
            }
            // 1ms pihenő a szálnak, hogy ne vigyük 100%-ra a CPU-t fölöslegesen
            tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
        }
    });

    // A main loop blokkol, amíg ki nem lépünk (Ctrl+C)
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
