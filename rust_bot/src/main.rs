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
    let signer = HyperliquidSigner::new(&app_config.private_key);
    let rest_client = HyperliquidClient::new();
    
    info!("🔑 Pénztárca cím: {}", signer.get_address());

    // 3. WebSocket Feed elindítása (Külön Tokio szálon fog pörögni)
    let feed = HyperliquidFeed::new(&coin);
    let state_ref = feed.state.clone();
    feed.start().await;

    use crate::logic::signal::SignalEngine;
    use crate::logic::order_manager::OrderManager;

    let mut simulated_balance_usd = 1000.0;
    
    // Százalék kiszámítása a 1000 dollárból, beállítva a maximum plafonnal (base_sz_usd)
    let calculated_usd = simulated_balance_usd * (app_config.strategy.balance_pct_per_trade / 100.0) * (app_config.strategy.leverage as f64);
    let target_usd = calculated_usd.min(app_config.strategy.base_sz_usd);
    
    info!("💰 Kereskedési méret beállítva: ${:.2} (Minden létra hossza)", target_usd);
    
    // 4. Szignál motor és Order Manager inicializálása
    let mut signal_engine = SignalEngine::new(app_config.strategy.clone(), state_ref.clone());
    let order_manager = OrderManager::new(app_config.strategy.clone());
    
    info!("⚙️ Kereskedési ciklus elindítva...");

    // 5. A fő "szívverés" (Heartbeat) - Extrém gyors polling az RwLock-ból
    // Mivel aszinkron és lock-free a state_ref, ezt nyugodtan pörgethetjük 1ms delay-el
    tokio::spawn(async move {
        loop {
            if let Some(signal) = signal_engine.tick().await {
                info!("🚨 SZIGNÁL ÉSZLELVE: {} @ {:.4}", signal.side, signal.target_mid);
                
                let payload = order_manager.build_ladder_payload(&signal.side, signal.target_mid, target_usd);
                
                info!("🛠️ Order Payload előkészítve: {}", serde_json::to_string(&payload).unwrap());

                // TODO: Sign payload (Signer) & Send via REST (Client)
                // A Pythonhoz képest ez itt < 1ms alatt fog megtörténni.

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
