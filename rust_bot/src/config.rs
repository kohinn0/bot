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
    /// TP minimum távolság a **belépési / ref ár** százalékában (pl. `1.0` = 1%).
    pub tp_min_pct: f64,
    /// SL minimum távolság a ref ár százalékában (pl. `1.25` = 1,25%).
    pub sl_min_pct: f64,
    pub max_positions: u32,
    pub dust_limit_usd: f64,
    pub min_signal_interval_ms: u64,
    pub balance_pct_per_trade: f64,
    pub max_notional_usd_per_trade: f64,
    /// Egy létraszint legkisebb USD értéke; alatta nem teszünk ki megbízást (díj vs edge).
    #[serde(default = "default_min_ladder_order_notional_usd")]
    pub min_ladder_order_notional_usd: f64,
    pub ladder_levels: Vec<LadderLevel>,

    // --- Liquidity Sweep ---
    #[serde(default = "default_sweep_window")]
    pub sweep_window: u32,
    #[serde(default = "default_sweep_threshold_pct")]
    pub sweep_threshold_pct: f64,

    // --- Order Flow ---
    #[serde(default = "default_flow_window")]
    pub flow_window: u32,
    #[serde(default = "default_flow_threshold")]
    pub flow_threshold: f64,

    // --- Anchored VWAP ---
    #[serde(default = "default_vwap_session_hours")]
    pub vwap_session_hours: u32,
    #[serde(default = "default_vwap_deviation_pct")]
    pub vwap_deviation_pct: f64,

    // --- Regime Filter (EMA trend-szűrő) ---
    /// Gyors EMA periódusa (minták száma). 30s mintánál: 50 × 30s ≈ 25 perc.
    #[serde(default = "default_regime_fast_period")]
    pub regime_fast_period: u32,
    /// Lassú EMA periódusa. 30s mintánál: 200 × 30s ≈ 100 perc.
    #[serde(default = "default_regime_slow_period")]
    pub regime_slow_period: u32,
    /// Mintavételezési intervallum másodpercben (hány mp-enként frissül az EMA).
    #[serde(default = "default_regime_sample_secs")]
    pub regime_sample_secs: u64,
    /// EMA-különbség sávszélessége (%) — ennél kisebb spread = SIDEWAYS (mindkét irány szabad).
    #[serde(default = "default_regime_band_pct")]
    pub regime_band_pct: f64,
}

fn default_min_ladder_order_notional_usd() -> f64 { 11.0 }
fn default_sweep_window() -> u32 { 100 }
fn default_sweep_threshold_pct() -> f64 { 0.08 }
fn default_flow_window() -> u32 { 50 }
fn default_flow_threshold() -> f64 { 3.5 }
fn default_vwap_session_hours() -> u32 { 8 }
fn default_vwap_deviation_pct() -> f64 { 0.15 }
fn default_regime_fast_period() -> u32 { 50 }
fn default_regime_slow_period() -> u32 { 200 }
fn default_regime_sample_secs() -> u64 { 30 }
fn default_regime_band_pct() -> f64 { 0.08 }

#[derive(Deserialize, Debug, Clone)]
pub struct LadderLevel {
    pub level: u32,
    pub offset_from_mid_pct: f64,
    pub size_pct: f64,
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
            tp_min_pct: 1.0,
            sl_min_pct: 1.0,
            max_positions: 1,
            dust_limit_usd: 15.0,
            min_signal_interval_ms: 0,
            balance_pct_per_trade: balance_pct,
            max_notional_usd_per_trade: max_n,
            min_ladder_order_notional_usd: 11.0,
            ladder_levels: vec![],
            sweep_window: 100,
            sweep_threshold_pct: 0.08,
            flow_window: 50,
            flow_threshold: 3.5,
            vwap_session_hours: 8,
            vwap_deviation_pct: 0.15,
            regime_fast_period: 50,
            regime_slow_period: 200,
            regime_sample_secs: 30,
            regime_band_pct: 0.08,
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
