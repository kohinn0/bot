# pyre-ignore-all-errors
from bot_logger import logger
import math
import time
from typing import Any, Optional, List, Dict, Tuple
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
        self.active_ladder: Any = None
    
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
        seen_prices: set = set()
        for level, price, size_pct in ladder_prices:
            size_raw = total_shares * size_pct
            size = max(float(config.min_shares), float(size_raw))

            szDecimals = 4
            factor = 10 ** szDecimals
            size = math.floor(size * factor) / factor

            # ── Dedup: ha két szint azonos áron landol (nagy tick_size esetén) ──
            # Összevonjuk az első szintbe és kihagyjuk a dupát.
            # Ez megakadályozza, hogy 2 identikus ordert küldjünk az API-ra.
            if price in seen_prices:
                # Keressük az első szintet ezen az áron, és növeljük a méretét
                for existing in orders:
                    if existing.price == price:
                        existing.size = math.floor((existing.size + size) * factor) / factor
                        logger.warning(
                            f"⚠️ DEDUP: Level {level} (${price:.2f}) ütközik Level {existing.level}-vel! "
                            f"Összevont méret: {existing.size}"
                        )
                        break
                continue
            seen_prices.add(price)

            order = LadderOrder(level=level, price=float(price), size=size)
            orders.append(order)


        if not orders:
            logger.error("❌ place_ladder: Nincs érvényes szint az árak dedup után! Visszatérés.")
            return None

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
                        elif "error" in st:
                            orders[i].order_id = f"ALO_REJECT_"
                            logger.info(f"⚠️ Rendelés ALO (Post-Only) miatt visszadobva: {st['error']}")
                        else:
                            orders[i].order_id = f"ERR_{i}"
                            
                    # Ha minden szint ALO_REJECT-et kapott, azt bot.py check_szál kezelje (üres ids)
                else:
                    for order in orders:
                        order.order_id = f"ERR_{order.level}"

            except Exception as e:
                logger.info(f"⚠️  Real batch order placement failed: {e}")
                for order in orders:
                    order.order_id = f"ERR_{order.level}"
        
        self.active_ladder = ladder
        return ladder
    
    def check_virtual_fills(self, current_mid: float) -> Tuple[bool, float, float]:
        """
        Dry Run módban szimulálja a kitöltést a WebSocket ár alapján.
        """
        if not self.dry_run or not self.active_ladder:
            return (False, 0.0, 0.0)

        ladder = self.active_ladder
        side = ladder.side
        filled_levels = 0
        total_qty = 0.0
        total_cost = 0.0

        for level in ladder.orders:
            if not level.filled:
                # LONG esetén: ha az ár lemegy a limitig vagy alá -> FILL
                if side == "LONG" and current_mid <= level.price:
                    level.filled = True
                    level.filled_size = level.size
                    logger.info(f"👻 GHOST FILL: LONG szint kitöltve @ ${level.price:.2f}")
                
                # SHORT esetén: ha az ár felmegy a limitig vagy fölé -> FILL
                elif side == "SHORT" and current_mid >= level.price:
                    level.filled = True
                    level.filled_size = level.size
                    logger.info(f"👻 GHOST FILL: SHORT szint kitöltve @ ${level.price:.2f}")

            if level.filled:
                filled_levels += 1
                total_qty += level.size
                total_cost += level.size * level.price

        if filled_levels > 0:
            avg_price = total_cost / total_qty
            ladder.total_size_filled = total_qty
            ladder.avg_fill_price = avg_price
            return (True, total_qty, avg_price)
        
        return (False, 0.0, 0.0)

    def check_fills(self) -> Tuple[bool, float, float]:
        if not self.active_ladder:
            return (False, 0.0, 0.0)
        
        if self.dry_run:
            # A szimuláció hívását a fő bot hurok vezérli (check_virtual_fills)
            return (False, 0.0, 0.0)
        
        total_filled: float = 0.0
        weighted_price_sum: float = 0.0
        
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
                    total_filled += float(order.size)
                    weighted_price_sum += float(order.price) * float(order.size)
                    
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
                cancels = [{"coin": str(self.active_ladder.coin), "o": int(str(oid))} for oid in unfilled_ids]
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
        slippage_penalty = tick_size * 2 # To prevent ALO rejection, pad by 2 ticks off mid

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
        self.tp_order_id: Any = None
        self.entry_price: Any = None
        self.entry_time: Any = None
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
        if self.tp_order_id and str(self.tp_order_id) != "DRY_TP" and not str(self.tp_order_id).startswith("ERR_"):
            if not self.dry_run and self.hl_client.exchange:
                try:
                    self.hl_client.exchange.cancel(self.coin, int(str(self.tp_order_id)))
                    logger.info(f"✅ LIVE TP order törölve: {self.tp_order_id}")
                except Exception as e:
                    logger.info(f"⚠️ TP cancel hiba: {e}")
        
        self.tp_order_id = None
        
    def close_position_at_market(self) -> bool:
        if self.dry_run:
            logger.info("🧪 [DRY RUN] Zárás piaci áron/simulált P&L elérése...")
            self.cancel_exit_orders()
            self._reset_state()
            return True

        self.cancel_exit_orders()
        
        # Determine actual size from API to handle partial fills with RETRY logic
        actual_size = 0.0
        state_success = False
        
        if self.hl_client.wallet and self.hl_client.info:
            for attempt in range(1, 4):
                try:
                    state = self.hl_client.info.user_state(self.hl_client.wallet.address)
                    positions = state.get("assetPositions", [])
                    for pos in positions:
                        if pos["position"]["coin"] == self.coin:
                            actual_size = float(pos["position"]["szi"])
                            break
                    state_success = True
                    break # Success, exit retry loop
                except Exception as e:
                    logger.warning(f"⚠️ Hiba a pozíció lekérdezésében (attempt {attempt}/3): {e}")
                    if attempt < 3:
                        time.sleep(0.3 * (2 ** (attempt - 1))) # 0.3s, 0.6s
                    else:
                        logger.error("❌ Pozíció állapot lekérése TARTÓSAN sikertelen a panic alatt.")
                
        if not state_success:
            logger.error("⚠️ Nem futtatható le a Market Close megerősített méret hiányában. Manuális beavatkozás szükséges!")
            self._reset_state()
            return False
            
        if abs(actual_size) > 0:
            is_buy = actual_size < 0  # if short, we buy to close
            sz = abs(actual_size)
            logger.info(f"🔥 MARKET CLOSE INDÍTÁSA - {self.coin} | Irány: {'BUY' if is_buy else 'SELL'} | Méret: {sz}")
            
            if self.hl_client.exchange:
                for attempt in range(1, 4):
                    try:
                        res = self.hl_client.exchange.market_open(
                            coin=self.coin,
                            is_buy=is_buy,
                            sz=sz,
                            px=None,
                            slippage=0.05
                        )
                        logger.info(f"✅ Market Close sikeres: {res}")
                        break # Success
                    except Exception as e:
                        logger.warning(f"⚠️ Market Close hiba (attempt {attempt}/3): {e}")
                        if attempt < 3:
                            time.sleep(0.5 * (2 ** (attempt - 1))) # 0.5s, 1.0s
                        else:
                            logger.error("❌ Market Close (Pánik gomb) VÉGLEGESEN ELSZÁLLT! Kézi zárás kell.")
        else:
             logger.info("ℹ️ Nincs nyitott pozíció a hálózaton. Nincs mit zárni.")
             
        self._reset_state()
        return True

    def _reset_state(self):
        self.entry_price = None
        self.entry_time = None
        self.position_size = 0.0

    def check_exit_conditions(self) -> Tuple[bool, str]:
        if not self.entry_time:
            return (False, "")

        params = config.get_time_stop_params()
        hold_time_sec = time.time() - float(self.entry_time)
        safe_fallback_timeout = params.get('expiration_sec', 12)

        if hold_time_sec > safe_fallback_timeout:
            return (True, "TIME_STOP_TIMEOUT")

        # HL API TP Fill check via info API
        if not self.dry_run and self.tp_order_id and not str(self.tp_order_id).startswith("ERR_"):
            try:
                open_orders = self.hl_client.info.open_orders(self.hl_client.wallet.address)
                open_oids = {str(o["oid"]) for o in open_orders}
                if self.tp_order_id not in open_oids:
                    return (True, "TAKE_PROFIT_REACHED")
            except Exception as e:
                logger.info(f"⚠️ Open orders ellenőrzése sikertelen az Exit checks-nél: {e}")

        return (False, "")

    def close_position_two_stage(self, current_price: float, tick_size: float) -> bool:
        """
        Kétlépcsős time-stop zárás (mentor javaslat):
        1. Először agresszív post-only limit ordert próbálunk  (spread szélére)
        2. Ha 800ms alatt nem tölt be → market close
        Ez ment megj a 0.025% taker fee-t a legtöbb time-stop esetben.
        """
        ts_cfg = config._config.get('order_management', {}).get('exit', {}).get('time_stop', {})
        action = ts_cfg.get('action_on_timeout', 'close_at_market')

        if action != 'try_aggressive_limit_then_market' or self.dry_run:
            # Dry run vagy legacy config: egyből market
            logger.info("[TWO_STAGE] Dry run / legacy → direct market close")
            return self.close_position_at_market()

        # --- 1. Lépés: Agresszív limit ---
        offset_ticks = ts_cfg.get('aggressive_limit_offset_ticks', 1)
        wait_ms = ts_cfg.get('aggressive_limit_wait_ms', 800)
        # Ha long pozíciónk van, szállunk ki SELL limit állal bid+1 tickre
        closing_price = round(current_price + tick_size * offset_ticks, 2)
        logger.info(
            f"[TWO_STAGE] TIME_STOP: agresszív limit kizárási árasánt → ${closing_price} ({wait_ms}ms várakozás)"
        )
        aggressive_oid = None
        try:
            if self.hl_client.exchange:
                sz = round(float(self.position_size), 4)
                res = self.hl_client.exchange.order(
                    self.coin, False, sz, closing_price,
                    {"limit": {"tif": "Alo"}}
                )
                aggressive_oid = str(res.get('response', {}).get('data', {}).get('statuses', [{}])[0].get('resting', {}).get('oid', ''))
                logger.info(f"[TWO_STAGE] Agresszív limit kiküldve: oid={aggressive_oid}")
        except Exception as e:
            logger.warning(f"[TWO_STAGE] Limit kiküldés sikertelen: {e} → market close")
            return self.close_position_at_market()

        # --- 2. Várakozás ---
        time.sleep(wait_ms / 1000.0)

        # --- 3. Ellenőrzzük, be töltött-e ---
        try:
            open_orders = self.hl_client.info.open_orders(self.hl_client.wallet.address)
            open_oids = {str(o["oid"]) for o in open_orders}
            if aggressive_oid and aggressive_oid not in open_oids:
                logger.info("[TWO_STAGE] ✅ Agresszív limit betöltött! Taker fee megtakarítva.")
                self._reset_state()
                return True
        except Exception as e:
            logger.warning(f"[TWO_STAGE] Fill check hiba: {e}")

        # --- 4. Fallback: piaci ár ---
        logger.warning("[TWO_STAGE] Limit nem töltött be → market close fallback")
        # Töröljük az agresszív limitet először
        if aggressive_oid and self.hl_client.exchange:
            try:
                self.hl_client.exchange.cancel(self.coin, int(aggressive_oid))
            except Exception:
                pass
        return self.close_position_at_market()
