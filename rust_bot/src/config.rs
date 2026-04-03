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
    pub min_signal_interval_ms: u64,
    pub balance_pct_per_trade: f64,
    pub max_notional_usd_per_trade: f64,
    /// Egy létraszint legkisebb USD értéke; alatta nem teszünk ki megbízást (díj vs edge).
    #[serde(default = "default_min_ladder_order_notional_usd")]
    pub min_ladder_order_notional_usd: f64,
    pub ladder_levels: Vec<LadderLevel>,
    pub signals: SignalConfig,
}

fn default_min_ladder_order_notional_usd() -> f64 {
    18.0
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
    /// Cél-notional (USD): `equity × (balance_pct/100) × leverage`, majd `max_notional` plafon.
    /// A HL perpnél a kitettség tipikusan a tőkeáttétellel skálázik; korábban csak a margin-összeg
    /// ment USD célba, ezért 1 SOL / többszintes létra irreálisan kicsi maradt.
    pub fn notional_per_level_usd(&self, equity: f64) -> f64 {
        let margin_usd = equity * (self.balance_pct_per_trade / 100.0);
        let notional = margin_usd * f64::from(self.leverage);
        notional.min(self.max_notional_usd_per_trade)
    }

    /// A legkisebb létraszelet USD-ben (target notional × legkisebb size_pct).
    pub fn min_ladder_slice_usd(&self, target_notional_usd: f64) -> f64 {
        self.ladder_levels
            .iter()
            .map(|l| target_notional_usd * l.size_pct)
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or(0.0)
    }
}

impl AppConfig {
    pub fn load() -> Self {
        let env_pk = std::env::var("HL_PRIVATE_KEY")
            .or_else(|_| std::env::var("PRIVATE_KEY"))
            .unwrap_or_default()
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        let env_user = std::env::var("HL_USER_ADDRESS").ok();
        let is_mainnet = std::env::var("IS_MAINNET").unwrap_or_else(|_| "true".to_string()) == "true";
        
        let file_content = fs::read_to_string("strategy_maker.json").expect("❌ Nem találom a strategy_maker.json-t!");
        let strat_config: StrategyConfig = serde_json::from_str(&file_content).expect("❌ Hibás JSON formátum!");

        // HL_PERP_DEX: üres = normál Hyperliquid perp; builder DEX esetén állítsd .env-ben
        let hl_perp_dex = std::env::var("HL_PERP_DEX").unwrap_or_default();

        // Fallback / DRY_RUN méret; élesben a HL accountValue felülírja, ha USE_WALLET_BALANCE_FOR_SIZING=true
        let starting_equity_usd = std::env::var("STARTING_EQUITY_USD")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(50.0);

        AppConfig {
            private_key: env_pk,
            hl_user_address: env_user,
            is_mainnet,
            hl_perp_dex,
            starting_equity_usd,
            use_wallet_balance_for_sizing: true,
            is_dry_run: false,
            strategy: strat_config,
        }
    }
}

#[cfg(test)]
mod strategy_config_tests {
    use super::*;

    fn strat(leverage: u32, balance_pct: f64, max_n: f64) -> StrategyConfig {
        StrategyConfig {
            coin: "SOL".to_string(),
            leverage,
            is_isolated: true,
            min_tick_size: 0.001,
            min_shares: 1.0,
            maker_fee_rate: 0.0,
            taker_fee_rate: 0.0,
            skew_penalty: None,
            tp_min_ticks: 1.0,
            sl_min_ticks: 1.0,
            max_positions: 1,
            dust_limit_usd: 15.0,
            min_signal_interval_ms: 0,
            balance_pct_per_trade: balance_pct,
            max_notional_usd_per_trade: max_n,
            min_ladder_order_notional_usd: 18.0,
            ladder_levels: vec![],
            signals: SignalConfig {
                z_score: ZScoreConfig {
                    enabled: false,
                    threshold: 0.0,
                    window: 1,
                },
                rsi: RsiConfig {
                    enabled: false,
                    window: 1,
                    buy_below: 0.0,
                    sell_above: 0.0,
                },
                bollinger: BollingerConfig {
                    enabled: false,
                    window: 1,
                    std_dev: 0.0,
                },
            },
        }
    }

    #[test]
    fn notional_per_level_usd_scales_with_leverage_and_caps() {
        let s = strat(10, 100.0, 800.0);
        assert!((s.notional_per_level_usd(80.0) - 800.0).abs() < 1e-6);
        let s2 = strat(10, 100.0, 500.0);
        assert!((s2.notional_per_level_usd(80.0) - 500.0).abs() < 1e-6);
        let s3 = strat(10, 50.0, 800.0);
        assert!((s3.notional_per_level_usd(80.0) - 400.0).abs() < 1e-6);
    }
}
