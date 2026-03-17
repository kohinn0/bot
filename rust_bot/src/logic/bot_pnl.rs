use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use tracing::{info, error};

#[derive(Debug, Serialize, Deserialize)]
pub struct PnlState {
    pub cumulative_pnl: f64,
    pub win_count: u32,
    pub loss_count: u32,
    pub total_fees: f64,
    pub account_value: f64,
    pub trades: Vec<f64>,
}

impl Default for PnlState {
    fn default() -> Self {
        Self {
            cumulative_pnl: 0.0,
            win_count: 0,
            loss_count: 0,
            total_fees: 0.0,
            account_value: 99.0, // Default for dry run
            trades: vec![0.0],
        }
    }
}

pub struct PnlTracker {
    file_path: String,
    state: PnlState,
}

impl PnlTracker {
    pub fn new(file_path: &str) -> Self {
        let mut tracker = Self {
            file_path: file_path.to_string(),
            state: PnlState::default(),
        };
        tracker.load_state();
        tracker
    }

    fn load_state(&mut self) {
        let path = Path::new(&self.file_path);
        
        // Ensure directory exists
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                let _ = fs::create_dir_all(parent);
            }
        }

        if path.exists() {
            match fs::read_to_string(path) {
                Ok(content) => match serde_json::from_str(&content) {
                    Ok(state) => self.state = state,
                    Err(e) => error!("⚠️ Hiba a PnL json parseolásakor: {}", e),
                },
                Err(e) => error!("⚠️ Hiba a PnL file olvasásakor: {}", e),
            }
        } else {
            self.save_state(self.state.account_value);
        }
    }

    pub fn save_state(&self, account_value: f64) {
        let state_to_save = PnlState {
            cumulative_pnl: self.state.cumulative_pnl,
            win_count: self.state.win_count,
            loss_count: self.state.loss_count,
            total_fees: self.state.total_fees,
            account_value,
            trades: self.state.trades.clone(),
        };

        match serde_json::to_string_pretty(&state_to_save) {
            Ok(json_str) => {
                if let Err(e) = fs::write(&self.file_path, json_str) {
                    error!("⚠️ Nem sikerült elmenteni a PnL állapotot: {}", e);
                }
            }
            Err(e) => error!("⚠️ Hiba a PnL json generálásakor: {}", e),
        }
    }

    pub fn add_trade(&mut self, profit_usd: f64, fee_usd: f64, current_account_value: f64) {
        let net_profit = profit_usd - fee_usd;
        self.state.cumulative_pnl += net_profit;
        self.state.total_fees += fee_usd;

        if net_profit > 0.0 {
            self.state.win_count += 1;
        } else {
            self.state.loss_count += 1;
        }

        self.state.trades.push(self.state.cumulative_pnl);
        self.save_state(current_account_value);

        info!(
            "💎 TRADE ZÁRVA | Net PnL: ${:.2} | Új Egyenleg: ${:.2}",
            net_profit, self.state.cumulative_pnl
        );
    }
}
