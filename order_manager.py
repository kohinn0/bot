"""
Order Manager - Handles 3-level post-only ladder placement and management
"""
import time
from typing import Optional, List, Dict, Tuple
from dataclasses import dataclass
from polymarket_client import PolymarketClient
from fee_utils import fetch_fee_rate_bps
from config import config


@dataclass
class LadderOrder:
    """Single order in the ladder"""
    level: int
    price: float
    size: int
    token_id: str
    order_id: Optional[str] = None
    filled: bool = False
    filled_size: int = 0


@dataclass
class LadderPosition:
    """Active ladder position"""
    side: str  # "UP" or "DOWN"
    token_id: str
    orders: List[LadderOrder]
    placed_at: float
    total_size_filled: int = 0
    avg_fill_price: float = 0.0


class OrderManager:
    """Manages post-only ladder orders"""
    
    def __init__(self, poly_client: PolymarketClient, dry_run: bool = True):
        self.poly_client = poly_client
        self.dry_run = dry_run
        self.active_ladder: Optional[LadderPosition] = None
    
    def place_ladder(
        self,
        token_id: str,
        side: str,  # "UP" or "DOWN"
        mid_price: float,
        total_shares: int,
        tick_size: float,
        inventory_yes: int = 0,
        inventory_no: int = 0,
    ) -> Optional[LadderPosition]:
        """
        Place 3-level post-only ladder and apply inventory risk penalty.
        
        Args:
            token_id: Token to buy
            side: "UP" or "DOWN"
            mid_price: Current mid price
            total_shares: Total shares to distribute across levels
            tick_size: Dynamic tick size for the current market
            inventory_yes: Current held UP shares
            inventory_no: Current held DOWN shares
        
        Returns:
            LadderPosition if successful, None otherwise
        """
        
        # Kalkuláljuk a spread büntetést az inventoryból
        penalty_ticks = config.get_inventory_risk_spread_penalty(inventory_yes, inventory_no, side)
        if penalty_ticks > 0:
            penalty_usd = penalty_ticks * tick_size
            print(f"   ⚠️ INVENTORY RISK: Aszimmetria büntetés érvényesítve: -{penalty_ticks} ticks (-${penalty_usd:.3f})")
            mid_price -= penalty_usd
            
        # Calculate ladder prices
        ladder_prices = config.calculate_ladder_prices(mid_price, "BUY", tick_size)
        
        # Create orders
        orders = []
        for level, price, size_pct in ladder_prices:
            size = int(total_shares * size_pct)
            if size < config.min_shares:
                size = config.min_shares
            
            order = LadderOrder(
                level=level,
                price=price,
                size=size,
                token_id=token_id
            )
            orders.append(order)
        
        # Create ladder position
        ladder = LadderPosition(
            side=side,
            token_id=token_id,
            orders=orders,
            placed_at=time.time()
        )
        
        # Place orders
        if self.dry_run:
            print(f"🧪 [DRY RUN] Placing ladder for {side} token:")
            for order in orders:
                print(f"   Level {order.level}: {order.size} shares @ ${order.price:.2f}")
                order.order_id = f"DRY_{order.level}"
        else:
            # Generate batch for real API
            batch_request = []
            from py_clob_client.order_builder.constants import BUY
            
            for order in orders:
                batch_request.append({
                    "token_id": order.token_id,
                    "price": order.price,
                    "size": order.size,
                    "side": BUY,
                    "expiration": 0 # A létra GTX marad (nincs GTD)
                })
            
            responses = self.poly_client.place_batch_orders(batch_request, dry_run=False)
            
            if responses and len(responses) == len(orders):
                for i, order in enumerate(orders):
                    order.order_id = responses[i].get('orderID')
            else:
                print(f"⚠️  Real batch order placement partially or fully failed.")
                for order in orders:
                    order.order_id = f"ERR_{order.level}"
        
        self.active_ladder = ladder
        return ladder
    
    def check_fills(self) -> Tuple[bool, int, float]:
        """
        Check if any ladder orders have been filled.
        
        Returns:
            (any_filled, total_filled_size, avg_price)
        """
        if not self.active_ladder:
            return (False, 0, 0.0)
        
        # In dry run, fills are handled by bot.py shadow logic
        if self.dry_run:
            return (False, 0, 0.0)
        
        # LIVE: Poll each order's status via the CLOB API
        total_filled = 0
        weighted_price_sum = 0.0
        
        try:
            for order in self.active_ladder.orders:
                if order.filled or not order.order_id:
                    continue
                if order.order_id.startswith("ERR_"):
                    continue
                
                try:
                    resp = self.poly_client.client.get_order(
                        order.order_id
                    )
                    if resp:
                        status = resp.get('status', '')
                        filled_sz = int(
                            float(resp.get('size_matched', 0))
                        )
                        if filled_sz > 0:
                            order.filled = True
                            order.filled_size = filled_sz
                            total_filled += filled_sz
                            weighted_price_sum += (
                                order.price * filled_sz
                            )
                except Exception as e:
                    print(f"\u26a0\ufe0f Fill check error for "
                          f"{order.order_id}: {e}")
        except Exception as e:
            print(f"\u26a0\ufe0f Bulk fill check error: {e}")
        
        if total_filled > 0:
            avg = weighted_price_sum / total_filled
            self.active_ladder.total_size_filled = total_filled
            self.active_ladder.avg_fill_price = avg
            return (True, total_filled, avg)
        
        return (False, 0, 0.0)
    
    def cancel_ladder(self) -> bool:
        """Cancel all unfilled ladder orders"""
        if not self.active_ladder:
            return False
            
        unfilled_ids = [o.order_id for o in self.active_ladder.orders if not o.filled and o.order_id]
        
        if self.dry_run:
            print(f"🧪 [DRY RUN] Canceling {len(unfilled_ids)} ladder orders")
            for order in self.active_ladder.orders:
                if not order.filled:
                    print(f"   Canceled Level {order.level}")
        else:
            if unfilled_ids:
                success = self.poly_client.cancel_batch_orders(unfilled_ids, dry_run=False)
                if success:
                    print(f"✅ Successfully canceled {len(unfilled_ids)} ladder orders via BATCH.")
                else:
                    print("❌ Failed to cancel some ladder orders.")
            else:
                 print("ℹ️ Nincsenek törlendő (unfilled) id-k a létrában.")
        
        self.active_ladder = None
        return True
    
    def has_active_ladder(self) -> bool:
        """Check if there's an active ladder"""
        return self.active_ladder is not None
    
    def get_ladder_age_ms(self) -> float:
        """Get age of current ladder in milliseconds"""
        if not self.active_ladder:
            return 0.0
        
        return (time.time() - self.active_ladder.placed_at) * 1000


class ExitManager:
    """Manages exit orders (TP, time-stop, reversal-stop)"""
    
    def __init__(self, poly_client: PolymarketClient, dry_run: bool = True):
        self.poly_client = poly_client
        self.dry_run = dry_run
        self.tp_order_id: Optional[str] = None
        self.entry_price: Optional[float] = None
        self.entry_time: Optional[float] = None
        self.position_size: int = 0
    
    def place_take_profit(
        self,
        token_id: str,
        entry_price: float,
        position_size: int,
        current_spread: float,
        tick_size: float,
        time_to_resolution_sec: int,
        toxicity_score: float = 0.0
    ) -> bool:
        """
        Place post-only take profit order GTD (Good-Til-Date) formátumban.
        
        Args:
            token_id: Token to sell
            entry_price: Average entry price
            position_size: Number of shares
            current_spread: Current bid-ask spread
            tick_size: Dynamic market tick size
            time_to_resolution_sec: GTD beállításhoz másodperc a lezárásig
        
        Returns:
            True if successful
        """
        # 1. Fast TP vs Slow TP logika (Adverse Selection védelem)
        is_toxic = toxicity_score >= 0.7
        
        if is_toxic:
            # Pánik/Mérgezett piac: "Fast TP"
            # Épphogy csak profitáljunk (1 tick), és nagyon gyorsan ejtsük (pár sec)
            tp_price = entry_price + tick_size
            expiration_sec = 3  # Gyors on-chain GTD halál
            print(f"   ⚠️ TOXIKUS PIAC (Score: {toxicity_score:.2f}) -> FAST TP aktiválva. Célár: ${tp_price:.3f}, GTD: 3s")
        else:
            # Normál ügymenet: "Slow TP"
            tp_price = config.calculate_take_profit_price(entry_price, current_spread, tick_size)
            # Másodpercalapú GTD idő kiszámolása a time_stop konfigurációkból
            params = config.get_time_stop_params(time_to_resolution_sec)
            expiration_sec = params.get('expiration_sec', 12)
        
        # Vigyázzunk az 1.0 limitre
        tp_price = min(0.99, tp_price)
        
        self.entry_price = entry_price
        self.entry_time = time.time()
        self.position_size = position_size
        
        if self.dry_run:
            print(f"🧪 [DRY RUN] Placing TP order (GTD: +{expiration_sec}s):")
            print(f"   Entry: ${entry_price:.2f}")
            print(f"   TP: ${tp_price:.2f} (+{(tp_price - entry_price):.2f})")
            print(f"   Size: {position_size} shares")
            self.tp_order_id = "DRY_TP"
            return True
        else:
            from py_clob_client.order_builder.constants import SELL
            
            tp_order = {
                "token_id": token_id,
                "price": tp_price,
                "size": position_size,
                "side": SELL,
                "expiration": expiration_sec # On-chain elhervadás 
            }
            
            responses = self.poly_client.place_batch_orders([tp_order], dry_run=False)
            
            if responses and len(responses) > 0:
                 self.tp_order_id = responses[0].get('orderID')
                 print(f"✅ Real TP GTD order placed! ID: {self.tp_order_id}")
                 return True
            else:
                 print(f"⚠️  Real TP order placement BATCH failed.")
                 self.tp_order_id = "ERR_TP"
                 return False
    
    def check_exit_conditions(self) -> Tuple[bool, str]:
        """
        Check if should exit manually. Because it's GTD, the engine handles timeouts mostly.
        But we can do manual reversal stops here.
        
        Returns:
            (should_exit, reason)
        """
        if not self.entry_time:
            return (False, "")
        
        # Mivel áttértünk GTD-re, a manually checkolt time-stop kevésbé fontos, 
        # de mint biztonsági fallback hagyjuk itt 2x akkora timeouttal.
        hold_time_sec = time.time() - self.entry_time
        safe_fallback_timeout = 60 # 60 sec absolut max
        
        if hold_time_sec > safe_fallback_timeout:
            return (True, "TIME_STOP_FALLBACK")
        
        # TODO: Check reversal stop
        
        return (False, "")
    
    def cancel_exit_orders(self):
        """Cancel all exit orders"""
        if self.tp_order_id:
            if self.dry_run:
                print(f"\ud83e\uddea [DRY RUN] Canceled TP order")
            else:
                # LIVE: Küldje el a törlési kérést az API-nak
                if not self.tp_order_id.startswith("ERR_"):
                    try:
                        self.poly_client.cancel_batch_orders(
                            [self.tp_order_id], dry_run=False
                        )
                        print(f"\u2705 LIVE TP order törölve: "
                              f"{self.tp_order_id}")
                    except Exception as e:
                        print(f"\u26a0\ufe0f TP cancel hiba: {e}")
        
        self.tp_order_id = None
        self.entry_price = None
        self.entry_time = None
        self.position_size = 0


if __name__ == "__main__":
    print("🧪 Testing Order Manager")
    print("=" * 60)
    
    # Mock client
    from polymarket_client import PolymarketClient
    client = PolymarketClient()
    
    # Create managers
    order_mgr = OrderManager(client, dry_run=True)
    exit_mgr = ExitManager(client, dry_run=True)
    
    # Test ladder placement
    print("\n1. Placing ladder:")
    ladder = order_mgr.place_ladder(
        token_id="test_token_123",
        side="DOWN",
        mid_price=0.48,
        total_shares=20,
        tick_size=0.01  # Mock tick size
    )
    
    if ladder:
        print(f"✅ Ladder placed with {len(ladder.orders)} levels")
    
    # Simulate wait
    time.sleep(2)
    
    # Check age
    age = order_mgr.get_ladder_age_ms()
    print(f"\n2. Ladder age: {age:.0f}ms")
    
    # Cancel ladder
    print(f"\n3. Canceling ladder:")
    order_mgr.cancel_ladder()
    
    # Test TP
    print(f"\n4. Placing TP:")
    exit_mgr.place_take_profit(
        token_id="test_token_123",
        entry_price=0.47,
        position_size=20,
        current_spread=0.04,
        tick_size=0.01          # Mock tick size
    )
    
    # Check exit conditions
    time.sleep(1)
    should_exit, reason = exit_mgr.check_exit_conditions(time_to_resolution_sec=300)
    print(f"\n5. Should exit: {should_exit} ({reason})")
    
    print(f"\n✅ Test complete")
