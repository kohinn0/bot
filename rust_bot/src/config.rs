use std::env;
use std::fs;
use tracing::info;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct LadderLevel {
    pub level: u32,
    pub offset_from_mid_ticks: i32,
    pub size_pct: f64,
}

#[derive(Debug, Clone)]
pub struct StrategyConfig {
    pub coin: String,
    pub leverage: u32,
    pub is_isolated: bool,
    pub base_sz_usd: f64,
    pub balance_pct_per_trade: f64,
    pub max_positions: u32,
    pub z_score_threshold: f64,
    pub sigma_r: f64,
    pub min_tick_size: f64,
    pub min_shares: f64,
    pub ladder_levels: Vec<LadderLevel>,
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

        let active_coin = env::var("ACTIVE_COIN").unwrap_or_else(|_| "SOL".to_string()).to_uppercase();

        // Load strategy_maker.json fallback paths
        let paths = ["strategy_maker.json", "../strategy_maker.json", "./rust_bot/strategy_maker.json"];
        let mut config_str = String::new();
        
        for path in paths.iter() {
            if let Ok(content) = fs::read_to_string(path) {
                config_str = content;
                break;
            }
        }
        
        if config_str.is_empty() {
            panic!("❌ Hiba: Vagy a 'strategy_maker.json' nem talalhato, vagy nincs jogosultsag olvasni!");
        }

        let parsed: Value = serde_json::from_str(&config_str).unwrap_or_else(|e| {
            panic!("❌ Hiba a config JSON parseolásakor: {}", e);
        });

        // Kinyerjük a belemélyesztett mezőket
        let rm = &parsed["risk_management"];
        let om = &parsed["order_management"];
        let tp = &parsed["technical_precision"];

        let mut ladder_levels = Vec::new();
        if let Some(arr) = om["entry"]["ladder_config"].as_array() {
            for item in arr {
                ladder_levels.push(LadderLevel {
                    level: item["level"].as_u64().unwrap_or(1) as u32,
                    offset_from_mid_ticks: item["offset_from_mid_ticks"].as_i64().unwrap_or(0) as i32,
                    size_pct: item["size_pct"].as_f64().unwrap_or(0.33),
                });
            }
        }

        let strategy = StrategyConfig {
            coin: active_coin,
            leverage: rm["leverage"]["max_leverage"].as_u64().unwrap_or(10) as u32,
            is_isolated: !rm["leverage"]["cross_margin"].as_bool().unwrap_or(false),
            base_sz_usd: rm["position_limits"]["max_notional_usd_per_trade"].as_f64().unwrap_or(50.0),
            balance_pct_per_trade: rm["position_limits"]["balance_pct_per_trade"].as_f64().unwrap_or(2.0),
            max_positions: rm["position_limits"]["max_open_positions"].as_u64().unwrap_or(1) as u32,
            
            z_score_threshold: om["entry"]["signal_params"]["z_threshold"].as_f64().unwrap_or(3.5),
            sigma_r: om["entry"]["sigma_multiplier"].as_f64().unwrap_or(1.5),
            
            min_tick_size: tp["tick_size"].as_f64().unwrap_or(0.01),
            min_shares: tp["min_shares"].as_f64().unwrap_or(0.001),
            ladder_levels,
            
            ping_pong_reprice: None,
            skew_penalty: Some(1.0),
        };

        info!("✅ Konfiguráció betöltve: {}, Dry Run: {}", strategy.coin, is_dry_run);

        Self {
            private_key,
            is_dry_run,
            strategy,
        }
    }
}
