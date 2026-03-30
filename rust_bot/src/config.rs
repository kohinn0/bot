use serde::Deserialize;
use std::fs;

#[derive(Deserialize, Debug, Clone)]
pub struct AppConfig {
    #[serde(default)]
    pub private_key: String,
    #[serde(default)]
    pub hl_user_address: Option<String>,
    #[serde(default)]
    pub is_mainnet: bool,
    #[serde(default)]
    pub hl_perp_dex: String,
    #[serde(default)]
    pub starting_equity_usd: f64,
    #[serde(default)]
    pub use_wallet_balance_for_sizing: bool,
    #[serde(default)]
    pub is_dry_run: bool,
    pub strategy: StrategyConfig,
}

#[derive(Deserialize, Debug, Clone)]
pub struct StrategyConfig {
    pub coin: String,
    pub leverage: u32,
    pub is_isolated: bool,
    pub min_tick_size: f64,
    pub min_shares: f64,
    pub maker_fee_rate: f64,
    pub taker_fee_rate: f64,
    pub skew_penalty: Option<f64>,
    pub tp_min_ticks: f64,
    pub sl_min_ticks: f64,
    pub max_positions: u32,
    pub dust_limit_usd: f64,
    pub pos_clear_threshold: f64,
    pub min_signal_interval_ms: u64,
    pub balance_pct_per_trade: f64,
    pub max_notional_usd_per_trade: f64,
    pub ladder_levels: Vec<LadderLevel>,
    pub signals: SignalConfig,
}

#[derive(Deserialize, Debug, Clone)]
pub struct LadderLevel {
    pub level: u32,
    pub offset_from_mid_pct: f64,
    pub size_pct: f64,
}

#[derive(Deserialize, Debug, Clone)]
pub struct SignalConfig {
    pub z_score: ZScoreConfig,
    pub rsi: RsiConfig,
    pub bollinger: BollingerConfig,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ZScoreConfig {
    pub enabled: bool,
    pub threshold: f64,
    pub window: usize,
}

#[derive(Deserialize, Debug, Clone)]
pub struct RsiConfig {
    pub enabled: bool,
    pub window: usize,
    pub buy_below: f64,
    pub sell_above: f64,
}

#[derive(Deserialize, Debug, Clone)]
pub struct BollingerConfig {
    pub enabled: bool,
    pub window: usize,
    pub std_dev: f64,
}

impl StrategyConfig {
    pub fn notional_per_level_usd(&self, equity: f64) -> f64 {
        let size = equity * (self.balance_pct_per_trade / 100.0);
        size.min(self.max_notional_usd_per_trade)
    }
}

impl AppConfig {
    pub fn load() -> Self {
        let env_pk = std::env::var("HL_PRIVATE_KEY").unwrap_or_default();
        let env_user = std::env::var("HL_USER_ADDRESS").ok();
        let is_mainnet = std::env::var("IS_MAINNET").unwrap_or_else(|_| "true".to_string()) == "true";
        
        let file_content = fs::read_to_string("strategy_maker.json").expect("❌ Nem találom a strategy_maker.json-t!");
        let strat_config: StrategyConfig = serde_json::from_str(&file_content).expect("❌ Hibás JSON formátum!");

        AppConfig {
            private_key: env_pk,
            hl_user_address: env_user,
            is_mainnet,
            hl_perp_dex: "https://api.hyperliquid.xyz".to_string(),
            starting_equity_usd: 100.0,
            use_wallet_balance_for_sizing: true,
            is_dry_run: false,
            strategy: strat_config,
        }
    }
}
