from bot_logger import logger
import math
import time
from typing import Optional, List, Dict, Tuple
from dataclasses import dataclass

from hyperliquid_client import HyperliquidClient
from config import config


@dataclass
class LadderOrder:
    level: int
    price: float
    size: float
    order_id: Optional[str] = None
    filled: bool = False
    filled_size: float = 0.0

@dataclass
class LadderPosition:
    side: str  # "LONG" or "SHORT"
    coin: str
    orders: List[LadderOrder]
    placed_at: float
    total_size_filled: float = 0.0
    avg_fill_price: float = 0.0

class OrderManager:
    """Manages post-only perpetual limits on Hyperliquid"""
    
    def __init__(
        self,
        hl_client: HyperliquidClient,
        dry_run: bool = True,
    ):
        self.hl_client = hl_client
        self.dry_run = dry_run
        self.active_ladder: Optional[LadderPosition] = None
    
    def place_ladder(
        self,
        coin: str,
        side: str,  # "LONG" or "SHORT"
        mid_price: float,
        total_usd_notional: float,
        tick_size: float,
        sigma_r: Optional[float] = None
    ) -> Optional[LadderPosition]:
        
        # 1) VOLA-ALAPÚ DINAMIKUS LÉTRA
        ladder_prices: List[Tuple[int, float, float]]
        if sigma_r is not None and sigma_r > 0:
            ladder_prices = self._build_vol_ladder(mid_price, side, tick_size, sigma_r)
        else:
            # Fallback
            # Notice the conversion: BUY -> LONG, SELL -> SHORT is handled in config now intuitively
            ladder_prices = config.calculate_ladder_prices(mid_price, "BUY" if side == "LONG" else "SELL", tick_size)
        
        # Calculate sizes based on USD notional size
        # HL size is in token amount
        total_shares = total_usd_notional / mid_price
        
        orders = []
        for level, price, size_pct in ladder_prices:
            size_raw = total_shares * size_pct
            size = max(config.min_shares, size_raw) # min shares should probably be min token amount for HL
            
            # HL typically requires specific size step formats, but we'll round to 4 decimals for now
            size = round(size, 4)
            
            order = LadderOrder(
                level=level,
                price=price,
                size=size,
            )
            orders.append(order)
        
        ladder = LadderPosition(
            side=side,
            coin=coin,
            orders=orders,
            placed_at=time.time()
        )
        
        if self.dry_run:
            logger.info(f"🧪 [DRY RUN] Placing ladder for {side} {coin}:")
            for order in orders:
                logger.info(f"   Level {order.level}: {order.size} {coin} @ ${order.price:.4f}")
                order.order_id = f"DRY_{order.level}"
        else:
            if not self.hl_client.exchange:
                return None
                
            batch = []
            is_buy = True if side == "LONG" else False
            
            for o in orders:
                batch.append({
                    "coin": coin,
                    "is_buy": is_buy,
                    "sz": o.size,
                    "limit_px": o.price,
                    "order_type": {"limit": {"tif": "Alo"}}, # GTX = ALO (Add Liquidity Only) in HL
                    "reduce_only": False
                })
                
            try:
                res = self.hl_client.exchange.bulk_orders(batch)
                logger.info(f"📤 BATCH ORDER RESPONSE: {res}")
                
                # HL response parsing
                # if res["status"] == "ok": extract OIDs
                # This requires proper HL response mapping which depends on the SDK version outputs
                if res and res.get("status") == "ok" and "response" in res and "data" in res["response"]:
                    statuses = res["response"]["data"]["statuses"]
                    for i, st in enumerate(statuses):
                        if "resting" in st:
                            orders[i].order_id = str(st["resting"]["oid"])
                        elif "filled" in st:
                            orders[i].order_id = str(st["filled"]["oid"])
                        else:
                            orders[i].order_id = f"ERR_{i}"
                else:
                    for order in orders:
                        order.order_id = f"ERR_{order.level}"

            except Exception as e:
                logger.info(f"⚠️  Real batch order placement failed: {e}")
                for order in orders:
                    order.order_id = f"ERR_{order.level}"
        
        self.active_ladder = ladder
        return ladder
    
    def check_fills(self) -> Tuple[bool, float, float]:
        if not self.active_ladder:
            return (False, 0.0, 0.0)
        
        if self.dry_run:
            return (False, 0.0, 0.0) # bot.py shadows this
        
        total_filled = 0.0
        weighted_price_sum = 0.0
        
        if not self.hl_client.wallet:
            return (False, 0.0, 0.0)
            
        try:
            # Info API polling open orders helps, but user_fills is better for exact details
            open_orders = self.hl_client.info.open_orders(self.hl_client.wallet.address)
            # Find which of our ladder orders are NO LONGER in open_orders
            open_oids = {str(o["oid"]) for o in open_orders}
            
            for order in self.active_ladder.orders:
                if order.filled or not order.order_id or order.order_id.startswith("ERR_"):
                    continue
                    
                if order.order_id not in open_oids:
                    # It's missing from open orders. Assume it filled!
                    # In a production bot you'd query user_fills to get the exact fill amount and price.
                    # For Sebesseg, missing from book = filled.
                    order.filled = True
                    order.filled_size = order.size
                    total_filled += order.size
                    weighted_price_sum += (order.price * order.size)
                    
        except Exception as e:
            logger.info(f"⚠️ Bulk fill check error: {e}")
        
        if total_filled > 0:
            avg = weighted_price_sum / total_filled
            self.active_ladder.total_size_filled += total_filled
            # Moving avg approximation
            if self.active_ladder.avg_fill_price == 0:
                self.active_ladder.avg_fill_price = avg
            else:
                self.active_ladder.avg_fill_price = (self.active_ladder.avg_fill_price + avg) / 2.0
                
            return (True, total_filled, avg)
        
        return (False, 0.0, 0.0)
    
    def cancel_ladder(self) -> bool:
        if not self.active_ladder:
            return False
            
        unfilled_ids = [o.order_id for o in self.active_ladder.orders if not o.filled and o.order_id and not o.order_id.startswith("ERR_")]
        
        if self.dry_run:
            logger.info(f"🧪 [DRY RUN] Canceling {len(unfilled_ids)} ladder orders")
        else:
            if unfilled_ids and self.hl_client.exchange:
                cancels = [{"coin": self.active_ladder.coin, "o": int(oid)} for oid in unfilled_ids]
                try:
                    res = self.hl_client.exchange.cancel(cancels)
                    logger.info(f"✅ Successfully canceled {len(unfilled_ids)} BATCH: {res}")
                except Exception as e:
                    logger.info(f"❌ Failed to cancel some ladder orders: {e}")
            else:
                 logger.info("ℹ️ Nincsenek törlendő (unfilled) id-k a létrában.")
        
        self.active_ladder = None
        return True

    def _build_vol_ladder(
        self,
        mid_price: float,
        side: str,
        tick_size: float,
        sigma_r: float,
    ) -> List[Tuple[int, float, float]]:
        gamma = 1.0  
        slippage_penalty = 0.0

        ladder: List[Tuple[int, float, float]] = []
        for level_cfg in config.ladder_levels:
            i = level_cfg.level
            
            # If LONG (BUY), price should be lower than mid
            # If SHORT (SELL), price should be higher than mid
            direction_mult = -1 if side == "LONG" else 1
            
            raw_price = mid_price * (1.0 + (direction_mult * i * gamma * sigma_r)) - (direction_mult * slippage_penalty)
            price = config.round_to_tick(raw_price, tick_size)
            ladder.append((level_cfg.level, price, level_cfg.size_pct))

        return ladder
    
    def get_ladder_age_ms(self) -> float:
        if not self.active_ladder:
            return 0.0
        return (time.time() - self.active_ladder.placed_at) * 1000

class ExitManager:
    def __init__(self, hl_client: HyperliquidClient, dry_run: bool = True):
        self.hl_client = hl_client
        self.dry_run = dry_run
        self.tp_order_id: Optional[str] = None
        self.entry_price: Optional[float] = None
        self.entry_time: Optional[float] = None
        self.position_size: float = 0.0
        self.coin: str = ""
        self.side: str = ""
    
    def place_take_profit(
        self,
        coin: str,
        side: str, # "LONG" or "SHORT"
        entry_price: float,
        position_size: float,
        current_spread: float,
        tick_size: float,
        toxicity_score: float = 0.0
    ) -> bool:
        is_toxic = toxicity_score >= 0.7
        
        # If we are LONG, TP is a SELL order. If SHORT, TP is a BUY order.
        tp_direction_mult = 1 if side == "LONG" else -1
        
        if is_toxic:
            tp_price = entry_price + (tp_direction_mult * tick_size)
            logger.info(f"   ⚠️ TOXIKUS PIAC (Score: {toxicity_score:.2f}) -> FAST TP aktiválva. Célár: ${tp_price:.3f}")
        else:
            profit = max(config.min_profit_ticks * tick_size, min(current_spread * config.spread_multiplier, config.max_profit_ticks * tick_size))
            tp_price = entry_price + (tp_direction_mult * profit)
            
        tp_price = config.round_to_tick(tp_price, tick_size)
        
        self.entry_price = entry_price
        self.entry_time = time.time()
        self.position_size = position_size
        self.coin = coin
        self.side = side
        
        if self.dry_run:
            logger.info(f"🧪 [DRY RUN] Placing TP order (Reduce Only):")
            logger.info(f"   Entry: ${entry_price:.2f}")
            logger.info(f"   TP: ${tp_price:.2f} ({side} EXIT)")
            logger.info(f"   Size: {position_size} {coin}")
            self.tp_order_id = "DRY_TP"
            return True
        else:
            if not self.hl_client.exchange:
                return False
                
            is_buy = False if side == "LONG" else True # TP side is opposite of entry side
            
            try:
                res = self.hl_client.exchange.order(
                    coin=coin,
                    is_buy=is_buy,
                    sz=position_size,
                    limit_px=tp_price,
                    order_type={"limit": {"tif": "Alo"}},
                    reduce_only=True
                )
                
                if res and res.get("status") == "ok":
                    statuses = res["response"]["data"]["statuses"]
                    if statuses and "resting" in statuses[0]:
                        self.tp_order_id = str(statuses[0]["resting"]["oid"])
                        logger.info(f"✅ Real TP order placed! ID: {self.tp_order_id}")
                        return True
            except Exception as e:
                logger.info(f"⚠️  Real TP order placement failed: {e}")
                
            self.tp_order_id = "ERR_TP"
            return False
            
    def cancel_exit_orders(self):
        if self.tp_order_id and self.tp_order_id != "DRY_TP" and not self.tp_order_id.startswith("ERR_"):
            if not self.dry_run and self.hl_client.exchange:
                try:
                    self.hl_client.exchange.cancel(self.coin, int(self.tp_order_id))
                    logger.info(f"✅ LIVE TP order törölve: {self.tp_order_id}")
                except Exception as e:
                    logger.info(f"⚠️ TP cancel hiba: {e}")
        
        self.tp_order_id = None
        self.entry_price = None
        self.entry_time = None
        self.position_size = 0.0

    def check_exit_conditions(self) -> Tuple[bool, str]:
        if not self.entry_time:
            return (False, "")
            
        params = config.get_time_stop_params()
        hold_time_sec = time.time() - self.entry_time
        safe_fallback_timeout = params.get('expiration_sec', 12)
        
        if hold_time_sec > safe_fallback_timeout:
            return (True, "TIME_STOP_TIMEOUT")
            
        # HL API TP Fill check via info API could also be placed here if bot.py loop relies on it
        
        return (False, "")
