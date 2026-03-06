# pyre-ignore-all-errors
import plotext as plt
from typing import List
from bot_logger import logger

class PnLTracker:
    def __init__(self):
        self.trades: List[float] = [0.0]  # Induló 0 egyenleg a grafikon elején
        self.cumulative_pnl: float = 0.0
        self.win_count: int = 0
        self.loss_count: int = 0
        self.total_fees: float = 0.0
    
    def add_trade(self, profit_usd: float, fee_usd: float):
        net_profit = profit_usd - fee_usd
        self.cumulative_pnl += net_profit
        self.total_fees += fee_usd
        
        if net_profit > 0:
            self.win_count += 1
        else:
            self.loss_count += 1
            
        self.trades.append(self.cumulative_pnl)
    
    def print_summary(self):
        total_trades = self.win_count + self.loss_count
        if total_trades == 0:
            return
            
        win_rate = (self.win_count / total_trades) * 100
        
        logger.info("📊 ========================================== 📊")
        logger.info("📊 NAPI PnL JELENTÉS (Dry Run Szimuláció)")
        logger.info("📊 ========================================== 📊")
        logger.info(f"📈 Összes Trade: {total_trades}")
        logger.info(f"🏆 Win Rate: {win_rate:.1f}% ({self.win_count} Win / {self.loss_count} Loss)")
        logger.info(f"💸 Kifizetett Taker díjak: ${self.total_fees:.2f}")
        logger.info(f"💎 NET PnL: ${self.cumulative_pnl:.2f} USD")
        logger.info("📊 ========================================== 📊")
        
        # Grafikon generálása a konzolra
        try:
            plt.clear_figure()
            plt.plot(self.trades, marker="dot", color="green" if self.cumulative_pnl >= 0 else "red")
            plt.title("Net PnL Alakulása (USD)")
            plt.xlabel("Trade Sorszám")
            plt.ylabel("USD ($)")
            
            # Sötét téma, terminálba illő plot
            plt.theme("dark")
            
            # Grafikon string sorokra bontása és logolása prefix nélkül (!)
            # hogy a grafikon ne csússzon el a dátumok miatt.
            # Trükk: logger.info helyett a bot_logger-en keresztül is átküldjük, de prefix mentes formázás kéne,
            # vagy egyszerűen csak simán formázzuk be. Eltoljuk beljebb a log prefix után.
            plot_str = plt.build()
            
            logger.info("\n" + plot_str)
        except Exception as e:
            logger.error(f"Grafikon hiba: {e}")

# Globális PnL Tracker példány
pnl_tracker = PnLTracker()
