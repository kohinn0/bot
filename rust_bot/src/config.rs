use serde::Deserialize;
use std::env;
use std::fs;
use tracing::info;

#[derive(Debug, Deserialize, Clone)]
pub struct StrategyConfig {
    pub coin: String,
    pub leverage: u32,
    pub is_isolated: bool,
    pub base_sz_usd: f64,
    pub max_positions: u32,
    pub z_score_threshold: f64,
    pub sigma_r: f64,
    pub min_tick_size: f64,
    pub ping_pong_reprice: Option<bool>,
    pub skew_penalty: Option<f64>,
}

pub struct AppConfig {
    pub private_key: String,
    pub is_dry_run: bool,
    pub strategy: StrategyConfig,
}

impl AppConfig {
    pub fn load() -> Self {
        // Load .env variables
        let private_key = env::var("PRIVATE_KEY").unwrap_or_else(|_| {
            panic!("❌ PRIVATE_KEY nem talalhato a .env fájlban!");
        });

        let is_dry_run = env::var("DRY_RUN")
            .unwrap_or_else(|_| "true".to_string())
            .to_lowercase() == "true";

        // Load strategy_maker.json
        let config_path = "../strategy_maker.json"; // Rust bot a bot mappa belsejében van
        let config_str = fs::read_to_string(config_path).unwrap_or_else(|_| {
            panic!("❌ Hiba a {} olvasásakor!", config_path);
        });

        let strategy: StrategyConfig = serde_json::from_str(&config_str).unwrap_or_else(|e| {
            panic!("❌ Hiba a config JSON parseolásakor: {}", e);
        });

        info!("✅ Konfiguráció betöltve: {}, Dry Run: {}", strategy.coin, is_dry_run);

        Self {
            private_key,
            is_dry_run,
            strategy,
        }
    }
}
