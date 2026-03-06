"""
Configuration module for Crypto Sebesseg Maker Strategy
Loads strategy from JSON and provides typed access to configuration.
"""
import os
import json
import math
from typing import Dict, List, Any, Optional, Tuple
from dataclasses import dataclass
from pathlib import Path
from dotenv import load_dotenv

# Load environment variables
load_dotenv()

# Path to strategy configs
CONFIG_DIR = Path(__file__).parent
STRATEGY_MAKER_PATH = CONFIG_DIR / "strategy_maker.json"
STRATEGY_TAKER_PATH = CONFIG_DIR / "strategy_taker.json"


@dataclass
class LadderLevel:
    level: int
    offset_from_mid_ticks: int
    size_pct: float
    comment: str


@dataclass
class ToxicFlowDetection:
    enabled: bool
    binance_velocity_threshold_pct_per_sec: float
    sustained_duration_ms: int
    action_if_toxic: str


@dataclass
class LeverageRisk:
    max_leverage: int
    cross_margin: bool


@dataclass
class RiskManagement:
    max_open_positions: int
    max_notional_usd_per_trade: float
    max_daily_loss_usd: float
    cooldown_after_exit_sec: int
    leverage: LeverageRisk


class MakerStrategyConfig:
    """Maker-only strategy configuration with ambush ladder"""
    
    def __init__(self, config_path: Path = STRATEGY_MAKER_PATH):
        with open(config_path, 'r', encoding='utf-8') as f:
            self._config = json.load(f)
        
        self._validate_config()
    
    def _validate_config(self):
        """Validate essential fields"""
        required = ['strategy_name', 'strategy_type', 'markets', 'signal_engine', 'order_management']
        for field in required:
            if field not in self._config:
                raise ValueError(f"Missing required config field: {field}")
    
    @property
    def strategy_name(self) -> str:
        return self._config['strategy_name']
    
    @property
    def strategy_type(self) -> str:
        return self._config['strategy_type']
    
    @property
    def philosophy(self) -> str:
        return self._config.get('philosophy', '')
    
    # === Markets ===
    @property
    def min_top_of_book_usd(self) -> float:
        return self._config['markets']['min_liquidity']['min_top_of_book_usd']
    
    @property
    def max_spread(self) -> float:
        return self._config['markets']['min_liquidity']['max_spread']
    
    @property
    def min_top_3_levels_usd(self) -> float:
        return self._config['markets']['min_liquidity']['min_top_3_levels_total_usd']
    
    @property
    def min_top_3_levels_usd(self) -> float:
        return self._config['markets']['min_liquidity']['min_top_3_levels_total_usd']
    
    # === Signal Engine ===
    @property
    def bearish_trigger_pct_range(self) -> Tuple[float, float]:
        r = self._config['signal_engine']['binance_triggers']['bearish']['price_drop_pct_range']
        return (r[0], r[1])
    
    @property
    def bullish_trigger_pct_range(self) -> Tuple[float, float]:
        r = self._config['signal_engine']['binance_triggers']['bullish']['price_rise_pct_range']
        return (r[0], r[1])
    
    @property
    def toxic_flow_detection(self) -> ToxicFlowDetection:
        tf = self._config['signal_engine']['adverse_selection_filter']['toxic_flow_detection']
        return ToxicFlowDetection(
            enabled=self._config['signal_engine']['adverse_selection_filter']['enabled'],
            binance_velocity_threshold_pct_per_sec=tf['binance_velocity_threshold_pct_per_sec'],
            sustained_duration_ms=tf['sustained_duration_ms'],
            action_if_toxic=tf['action_if_toxic']
        )
    
    def is_toxic_flow(self, velocity_pct_per_sec: float, duration_ms: int) -> bool:
        """Check if current Binance flow is toxic (likely informed trading)"""
        tf = self.toxic_flow_detection
        if not tf.enabled:
            return False
        
        return (velocity_pct_per_sec >= tf.binance_velocity_threshold_pct_per_sec and 
                duration_ms >= tf.sustained_duration_ms)
    
    def is_toxic_flow_advanced(
        self,
        dprice_per_sec_pct: float,
        volume_spike: float,
        atr: float,
        theta: float,
    ) -> bool:
        """
        Adaptív Toxic Flow filter a dokumentumban leírt formula szerint:

            |ΔP/Δt| * Volume_spike > Θ * ATR

        ahol:
            - dprice_per_sec_pct: árfolyamsebesség %/sec-ben
            - volume_spike: aktuális volumen / átlagos volumen
            - atr: Average True Range (átlagos gyertya/mozgás mérete)
            - theta (Θ): dinamikus küszöb szorzó
        """
        if not self.toxic_flow_detection.enabled:
            return False
        if atr <= 0:
            return False
        lhs = abs(dprice_per_sec_pct) * max(volume_spike, 0.0)
        rhs = theta * atr
        return lhs > rhs
    
    # === Ladder Configuration ===
    @property
    def ladder_levels(self) -> List[LadderLevel]:
        levels = []
        for cfg in self._config['order_management']['entry']['ladder_config']:
            levels.append(LadderLevel(
                level=cfg['level'],
                offset_from_mid_ticks=cfg['offset_from_mid_ticks'],
                size_pct=cfg['size_pct'],
                comment=cfg['comment']
            ))
        return levels
    
    def calculate_ladder_prices(self, mid_price: float, side: str, tick_size: float) -> List[Tuple[int, float, float]]:
        """
        Calculate ladder prices based on mid price.
        
        A lebegőpontos hibák elkerülése érdekében az árak alapja mindig
        floor(mid_price / tick_size) * tick_size, azaz a "rugóponthoz"
        legközelebbi érvényes tick. Az offseteket erre adjuk rá egész tickekben.
        """
        ladder = []

        # Az alap tick: a mid_price-t leráccsükjük az előző tick-re (float hiba-mentes alap)
        ticks_in_mid = math.floor(mid_price / tick_size)
        base_price = ticks_in_mid * tick_size

        # Decimals száma a tick_size-ból (pl. tick=1.0 → 0, tick=0.5 → 1, tick=0.1 → 1)
        str_tick = f"{tick_size:.10f}".rstrip('0')
        decimals = len(str_tick.split('.')[1]) if '.' in str_tick else 0

        for level_cfg in self.ladder_levels:
            offset_ticks = level_cfg.offset_from_mid_ticks

            if side == "BUY":
                raw_price = base_price + (offset_ticks * tick_size)
            else:  # SELL
                raw_price = base_price + (-offset_ticks * tick_size)

            # Utolsó preciziós kerekítés a float artéfaktumok ellen
            price = round(raw_price, decimals)

            ladder.append((level_cfg.level, price, level_cfg.size_pct))

        return ladder
    
    @property
    def wait_for_fill_ms(self) -> int:
        return self._config['order_management']['entry']['execution']['wait_for_fill_ms']
    
    @property
    def min_fill_pct(self) -> float:
        return self._config['order_management']['entry']['execution']['partial_fill_min_pct']
    
    @property
    def min_fill_absolute_shares(self) -> int:
        return self._config['order_management']['entry']['execution']['partial_fill_min_absolute_shares']
    
    # === Exit Configuration ===
    @property
    def min_profit_ticks(self) -> int:
        return self._config['order_management']['exit']['take_profit']['min_profit_ticks']
    
    @property
    def max_profit_ticks(self) -> int:
        return self._config['order_management']['exit']['take_profit']['max_profit_ticks']
    
    @property
    def spread_multiplier(self) -> float:
        return self._config['order_management']['exit']['take_profit']['spread_multiplier']
    
    def calculate_take_profit_price(self, entry_price: float, current_spread: float, tick_size: float) -> float:
        """
        Calculate take profit price: entry + max(2 ticks, 0.7 * spread)
        
        Args:
            entry_price: Actual fill price
            current_spread: Current bid-ask spread
            tick_size: Dynamic minimum price increment for the market
        
        Returns:
            Take profit price rounded to tick
        """
        min_profit = self.min_profit_ticks * tick_size
        max_profit = self.max_profit_ticks * tick_size
        spread_profit = current_spread * self.spread_multiplier
        
        profit = max(min_profit, min(spread_profit, max_profit))
        tp_price = entry_price + profit
        
        return round(tp_price / tick_size) * tick_size
    
    def get_time_stop_params(self) -> dict[str, float]:
        """
        Get time-stop parameters.
        
        Returns:
            {'expiration_sec': int, 'profit_multiplier': float, 'size_multiplier': float}
        """
        return {
            'expiration_sec': self._config['order_management']['exit']['time_stop'].get('base_expiration_sec', 12),
            'profit_multiplier': 1.0,
            'size_multiplier': 1.0
        }
    
    # === Risk Management ===
    @property
    def risk_management(self) -> RiskManagement:
        rm = self._config['risk_management']
        lev = rm.get('leverage', {})
        return RiskManagement(
            max_open_positions=rm['position_limits']['max_open_positions'],
            max_notional_usd_per_trade=rm['position_limits']['max_notional_usd_per_trade'],
            max_daily_loss_usd=rm['daily_limits']['max_daily_loss_usd'],
            cooldown_after_exit_sec=rm['cooldown']['after_exit_sec'],
            leverage=LeverageRisk(
                max_leverage=lev.get('max_leverage', 10),
                cross_margin=lev.get('cross_margin', False)
            )
        )
    @property
    def rpc_url(self) -> str:
        """Returns the configured RPC URL, essential for lower latencies."""
        bc = self._config.get('blockchain', {})
        return bc.get('rpc_url', os.getenv('POLY_RPC_URL', 'https://polygon-rpc.com'))
        
    # === Technical Precision ===
    @property
    def min_shares(self) -> int:
        return self._config['technical_precision']['min_shares']
    
    def round_to_tick(self, price: float, tick_size: float) -> float:
        """Round price to valid market tick size safely avoiding float precision artifacts"""
        ticks = round(price / tick_size)
        result = ticks * tick_size
        
        # Calculate maximum decimals needed by looking at tick_size string representation
        # 0.01 -> 2 decimals. 1.0 -> 0 decimals.
        str_tick = f"{tick_size:.10f}".rstrip('0')
        decimals = len(str_tick.split('.')[1]) if '.' in str_tick else 0
        
        return round(float(result), int(decimals)) # pyre-ignore[6]
    
    def validate_shares(self, shares: float) -> int:
        """Validate and round shares to minimum"""
        return max(self.min_shares, int(shares))
    
    # === Rate Limiting ===
    @property
    def min_ms_between_updates(self) -> int:
        return self._config['order_management']['rate_limiting']['min_ms_between_order_updates']
    
    @property
    def max_orders_per_minute(self) -> int:
        return self._config['order_management']['rate_limiting']['max_orders_per_minute']
    
    # === Environment Variables ===
    @property
    def private_key(self) -> str:
        key = os.getenv("PRIVATE_KEY")
        if not key:
            raise ValueError("PRIVATE_KEY not found in environment")
        return key
    
    @property
    def dry_run(self) -> bool:
        return os.getenv("DRY_RUN", "true").lower() == "true"


# Global config instance
config = MakerStrategyConfig()


if __name__ == "__main__":
    # Test configuration loading
    print(f"╔══════════════════════════════════════════════════════════╗")
    print(f"║  {config.strategy_name:^54}  ║")
    print(f"╠══════════════════════════════════════════════════════════╣")
    print(f"║  Type: {config.strategy_type:46}  ║")
    print(f"║  Philosophy: {config.philosophy:42}  ║")
    print(f"╚══════════════════════════════════════════════════════════╝\n")
    
    # Ladder configuration
    print("🪜 AMBUSH LADDER SETUP:")
    mid = 0.48
    tick_size = 0.01  # Mock tick size for testing
    ladder = config.calculate_ladder_prices(mid, "BUY", tick_size)
    for level, price, size_pct in ladder:
        print(f"  Level {level}: ${price:.2f} ({size_pct*100:.0f}% of size)")
    
    # Take profit calculation
    print(f"\n💰 TAKE PROFIT CALCULATION:")
    entry = 0.47
    spread = 0.04
    tp = config.calculate_take_profit_price(entry, spread, tick_size)
    profit_ticks = (tp - entry) / tick_size
    print(f"  Entry: ${entry:.2f}")
    print(f"  Spread: ${spread:.2f}")
    print(f"  TP: ${tp:.2f} (+{profit_ticks:.0f} ticks)")
    
    # Time-based scaling
    print(f"\n⏱️  TIME-STOP SCALING:")
    params = config.get_time_stop_params()
    print(f"  GTD expiration={params.get('expiration_sec')}sec, "
          f"profit_mult={params['profit_multiplier']:.1f}x, "
          f"size_mult={params['size_multiplier']:.1f}x")
    
    # Toxic flow check
    print(f"\n🛡️  TOXIC FLOW PROTECTION:")
    print(f"  Threshold: {config.toxic_flow_detection.binance_velocity_threshold_pct_per_sec}%/sec for "
          f"{config.toxic_flow_detection.sustained_duration_ms}ms")
    print(f"  Test: 0.3%/sec for 2500ms → Toxic? {config.is_toxic_flow(0.3, 2500)}")
    print(f"  Test: 0.2%/sec for 1500ms → Toxic? {config.is_toxic_flow(0.2, 1500)}")
    
    # Risk limits
    print(f"\n🎯 RISK LIMITS:")
    rm = config.risk_management
    print(f"  Max positions: {rm.max_open_positions}")
    print(f"  Max per trade: ${rm.max_notional_usd_per_trade}")
    print(f"  Daily loss limit: ${rm.max_daily_loss_usd}")
    
    print(f"\n✅ Configuration loaded successfully!")
