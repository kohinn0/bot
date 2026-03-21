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
    pub skew_penalty: Option<f64>,
    pub min_signal_interval_ms: u64,
    pub max_daily_loss_usd: f64,
    pub max_daily_trades: u32,
}

impl StrategyConfig {
    /// Létra szintenkénti cél-notional (USD): egyenleg % × tőkeáttétel, cap `base_sz_usd`.
    pub fn notional_per_level_usd(&self, equity_usd: f64) -> f64 {
        let computed =
            equity_usd * (self.balance_pct_per_trade / 100.0) * (self.leverage as f64);
        computed.min(self.base_sz_usd)
    }
}

pub struct AppConfig {
    pub private_key: String,
    pub is_dry_run: bool,
    pub is_mainnet: bool,
    /// Induló egyenleg (USD) — fallback, ha HL egyenleg nem kérhető le; env: STARTING_EQUITY_USD
    pub starting_equity_usd: f64,
    /// Ha true (alap): méret a Hyperliquid `clearinghouseState.accountValue` alapján; env: USE_WALLET_BALANCE_FOR_SIZING
    pub use_wallet_balance_for_sizing: bool,
    /// HL egyenleg frissítése másodpercenként (méret követése); env: WALLET_EQUITY_REFRESH_SEC
    pub wallet_equity_refresh_sec: u64,
    /// Builder / másodlagos perp DEX neve; env: HL_PERP_DEX (üres = alap HL perp, nincs `dex` mező a kérésben)
    pub hl_perp_dex: Option<String>,
    /// API / user stream cím (master), ha az aláíró (agent) kulcs más címről származik; env: HL_USER_ADDRESS
    pub hl_user_address: Option<String>,
    pub strategy: StrategyConfig,
}

impl AppConfig {
    pub fn load() -> Self {
        // Load .env variables
        let private_key = env::var("PRIVATE_KEY").unwrap_or_else(|_| {
            panic!("❌ PRIVATE_KEY nem talalhato a .env fájlban!");
        });

        let is_dry_run = env::var("DRY_RUN")
            .unwrap_or_else(|_| "false".to_string())
            .trim()
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
            
            skew_penalty: Some(1.0),
            min_signal_interval_ms: parsed["signal_engine"]["debounce"]["min_time_between_entries_ms"]
                .as_u64()
                .unwrap_or(8000),
            max_daily_loss_usd: rm["daily_limits"]["max_daily_loss_usd"].as_f64().unwrap_or(10.0),
            max_daily_trades: rm["daily_limits"]["max_daily_trades"].as_u64().unwrap_or(25) as u32,
        };

        info!("✅ Konfiguráció betöltve: {}, Dry Run: {}", strategy.coin, is_dry_run);

        let is_mainnet = env::var("IS_MAINNET")
            .unwrap_or_else(|_| "true".to_string())
            .trim()
            .to_lowercase() == "true";

        let starting_equity_usd = env::var("STARTING_EQUITY_USD")
            .ok()
            .and_then(|s| s.trim().parse::<f64>().ok())
            .filter(|&v| v > 0.0)
            .unwrap_or(100.0);

        let use_wallet_balance_for_sizing = env::var("USE_WALLET_BALANCE_FOR_SIZING")
            .unwrap_or_else(|_| "true".to_string())
            .trim()
            .to_lowercase()
            != "false";

        let wallet_equity_refresh_sec = env::var("WALLET_EQUITY_REFRESH_SEC")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|&v| v >= 10)
            .unwrap_or(30);

        let hl_perp_dex = env::var("HL_PERP_DEX")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let hl_user_address = env::var("HL_USER_ADDRESS")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        Self {
            private_key,
            is_dry_run,
            is_mainnet,
            starting_equity_usd,
            use_wallet_balance_for_sizing,
            wallet_equity_refresh_sec,
            hl_perp_dex,
            hl_user_address,
            strategy,
        }
    }
}
