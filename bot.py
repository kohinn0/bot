"""
Crypto Sebesseg - Main Trading Bot
Automated maker-only momentum scalping bot
"""
import time
import sys
import datetime
from enum import Enum
from dataclasses import dataclass
from typing import Optional

from config import config
from binance_feed import BinanceFeed
from market_finder import MarketFinder, MarketContext
from polymarket_client import PolymarketClient
from signal_engine import SignalEngine, PriceMove
from order_manager import OrderManager, ExitManager
from toxicity_engine import ToxicityEngine


class BotState(Enum):
    """Bot states"""
    IDLE = "IDLE"
    ARMED = "ARMED"
    LADDER_PLACED = "LADDER_PLACED"
    WAITING_FILL = "WAITING_FILL"
    IN_POSITION = "IN_POSITION"
    EXITING = "EXITING"
    COOLDOWN = "COOLDOWN"
    HALT = "HALT"


@dataclass
class BotStats:
    """Bot statistics"""
    signals_detected: int = 0
    ladders_placed: int = 0
    fills_received: int = 0
    exits_completed: int = 0
    no_fill_skips: int = 0
    total_pnl: float = 0.0


class TradingBot:
    """Main trading bot"""
    
    def __init__(self, dry_run: bool = True):
        self.dry_run = dry_run
        self.state = BotState.IDLE
        self.stats = BotStats()
        
        # Components
        self.binance: Optional[BinanceFeed] = None
        self.finder: Optional[MarketFinder] = None
        self.poly_client: Optional[PolymarketClient] = None
        self.signal_engine: Optional[SignalEngine] = None
        self.order_mgr: Optional[OrderManager] = None
        self.exit_mgr: Optional[ExitManager] = None
        self.toxicity_engine: Optional[ToxicityEngine] = None
        
        self.market: Optional[MarketContext] = None
        
        # Inventory tracking
        self.inventory_yes: int = 0
        self.inventory_no: int = 0
        
        # Analytics state
        self.last_signal_time: float = 0.0
        self.last_quote_time: float = 0.0
        
        # Cooldown
        self.cooldown_until: float = 0
        
        # Verbose logging
        self.last_price_log: float = 0
        self.last_logged_price: Optional[float] = None
        
        print(f"🤖 Crypto Sebesseg Bot")
        print(f"   Mode: {'DRY RUN' if dry_run else '🔴 LIVE'}")
        print(f"   Risk: ${config.risk_management.max_notional_usd_per_trade}/trade")
        print()
    
    def initialize(self) -> bool:
        """Initialize all components"""
        print("🔧 Initializing components...")
        
        # Binance feed
        print("  ├─ Binance WebSocket...", end=" ", flush=True)
        self.binance = BinanceFeed()
        self.binance.start()
        time.sleep(2)
        if not self.binance.get_current_price():
            print("❌")
            return False
        print("✅")
        
        # Market finder
        print("  ├─ Market finder...", end=" ", flush=True)
        self.finder = MarketFinder()
        print("✅")
        
        # Polymarket client wrapper
        print("  ├─ Polymarket client...", end=" ", flush=True)
        self.poly_client = PolymarketClient()
        if not self.dry_run:
            if not self.poly_client.initialize():
                print("❌")
                return False
        print("✅")
        
        # Signal engine
        print("  ├─ Signal engine...", end=" ", flush=True)
        self.signal_engine = SignalEngine(self.binance)
        print("✅")
        
        # Order manager
        print("  ├─ Order manager...", end=" ", flush=True)
        self.order_mgr = OrderManager(self.poly_client, dry_run=self.dry_run)
        print("✅")
        
        # Exit manager
        print("  ├─ Exit manager...", end=" ", flush=True)
        self.exit_mgr = ExitManager(self.poly_client, dry_run=self.dry_run)
        print("✅")
        
        # Toxicity engine
        print("  └─ Toxicity engine...", end=" ", flush=True)
        self.toxicity_engine = ToxicityEngine(log_dir="logs")
        print("✅")
        
        print()
        return True
    
    def find_market(self) -> bool:
        """Find active 15-min market"""
        print("🔍 Finding active market...", end=" ", flush=True)
        try:
            self.market = self.finder.get_active_market(self.binance)
            if self.market:
                print("✅")
                print(f"   Market: {self.market.slug[:50]}...")
                
                # Csatlakoztatás a WS auto-halthoz
                print("   🔗 Polymarket WS monitor csatlakoztatása...", end=" ", flush=True)
                tokens = [self.market.up_token_id, self.market.down_token_id]
                self.signal_engine.attach_polymarket_ws(tokens)
                print("✅")
                
                return True
            else:
                print("❌")
                return False
        except Exception as e:
            print(f"❌ {e}")
            return False
    
    def run(self):
        """Main trading loop"""
        if not self.initialize():
            print("❌ Initialization failed")
            return
        
        if not self.find_market():
            print("⚠️  No market found - bot will wait for markets")
        
        print()
        print("=" * 60)
        print("🚀 BOT STARTED - Monitoring for signals")
        print("=" * 60)
        
        # Market cycle tracking
        if self.market:
            import datetime
            time_to_res = (self.market.end - datetime.datetime.now(datetime.timezone.utc)).total_seconds()
            print(f"⏱️  Time to market resolution: {int(time_to_res)}s (~{int(time_to_res/60)} min)")
        
        print()
        
        self.state = BotState.ARMED
        
        cycle_start = time.time()
        last_stats = time.time()
        
        try:
            while True:
                self._tick()
                
                # Show stats every 60 seconds
                if time.time() - last_stats > 60:
                    elapsed = time.time() - cycle_start
                    signals_per_min = (self.stats.signals_detected / elapsed) * 60 if elapsed > 0 else 0
                    print(f"\n📈 STATS UPDATE ({int(elapsed/60)}min elapsed):")
                    print(f"   Signals: {self.stats.signals_detected} ({signals_per_min:.1f}/min)")
                    print(f"   Ladders: {self.stats.ladders_placed}")
                    print(f"   Fills: {self.stats.fills_received}")
                    print(f"   No-fills: {self.stats.no_fill_skips}")
                    print()
                    last_stats = time.time()
                
                time.sleep(0.1)  # 100ms tick
                
        except KeyboardInterrupt:
            print("\n\n🛑 Stopping bot...")
            self._shutdown()
    
    def _tick(self):
        """Single bot tick (called every 100ms)"""
        
        # 1. EMERGENCY AUTO-HALT CHECK (Minden állapotban ellenőrizzük)
        if self.signal_engine and self.signal_engine.check_auto_halt():
            print("\n🚨🚨 AUTO-HALT KIVÁLTVA! MÉRGES (TOXIC) VOLUMEN SPIKE DETEKTÁLVA! 🚨🚨")
            print("   Minden aktív pozíció és order azonnali visszavonása...")
            
            # Orderek törlése azonnal
            if self.order_mgr:
                self.order_mgr.cancel_ladder()
            if self.exit_mgr:
                self.exit_mgr.cancel_exit_orders()
            
            # Karantén
            self.state = BotState.HALT
            cooldown_min = 30
            self.cooldown_until = time.time() + (cooldown_min * 60)
            print(f"   ⏸️  Piac karanténba helyezve {cooldown_min} percre.")
            return

        # 2. Markout Engine Refresh (Telemetria)
        if self.toxicity_engine and self.state != BotState.IDLE:
            current_mid_prices = {}
            if self.market and self.toxicity_engine.pending_markouts:
                # Guard: csak ha van élő kliens (LIVE mód)
                has_client = (self.poly_client
                              and hasattr(self.poly_client, 'client')
                              and self.poly_client.client is not None)
                if has_client:
                    tokens_to_check = {self.market.up_token_id,
                                       self.market.down_token_id}
                    for token in tokens_to_check:
                        try:
                            book = self.poly_client.client.get_order_book(token)
                            if book and book.bids and book.asks:
                                bb = float(book.bids[0].price)
                                bs = float(book.bids[0].size)
                                ba = float(book.asks[0].price)
                                a_s = float(book.asks[0].size)
                                mid = (bb + ba) / 2.0
                                tot = bs + a_s
                                microprice = ((bb * a_s + ba * bs) / tot
                                              if tot > 0 else mid)
                                current_mid_prices[token] = {
                                    'mid': mid,
                                    'microprice': microprice
                                }
                            else:
                                fb = (self.poly_client.get_yes_price(token)
                                      or 0.50)
                                current_mid_prices[token] = {
                                    'mid': fb, 'microprice': fb
                                }
                        except Exception as e:
                            print(f"⚠️ Microprice fetch error: {e}")
                    
            self.toxicity_engine.update_markouts(current_mid_prices)
            
            # Dinamikus Toxicity Kiírás ha túl mérgező (és nem cooldownban vagyunk)
            if self.toxicity_engine.is_toxic() and self.state == BotState.ARMED:
                now = time.time()
                if now - self.last_price_log > 5.0:  # Ne spamelje tele a logot
                    print(f"\n⚠️  FIGYELEM: Toxikus order flow! Score: {self.toxicity_engine.toxicity_score:.2f} (Edge vesztés veszélye)")
                    self.last_price_log = now

        # State machine
        if self.state == BotState.ARMED:
            self._armed_tick()
        
        elif self.state == BotState.LADDER_PLACED:
            self._ladder_placed_tick()
        
        elif self.state == BotState.WAITING_FILL:
            self._waiting_fill_tick()
        
        elif self.state == BotState.IN_POSITION:
            self._in_position_tick()
        
        elif self.state == BotState.COOLDOWN:
            self._cooldown_tick()
            
        elif self.state == BotState.HALT:
            self._halt_tick()
    
    def _halt_tick(self):
        """Halt state - Piac karanténban mérgező flow miatt"""
        if time.time() >= self.cooldown_until:
            print(f"\n✅ Karantén lejárt - Vissza ARMED módba")
            self.state = BotState.ARMED
        
        # Ritkább logolás halt alatt (pl 10 másodpercenként)
        now = time.time()
        if now - self.last_price_log > 10.0:
            rem = int(self.cooldown_until - now)
            print(f"🔒 AUTO-HALT AKTÍV | Hátralévő karantén: {rem} másodperc")
            self.last_price_log = now
    
    def _armed_tick(self):
        """Armed state - waiting for signal"""
        
        # Guardrail ellenőrzés a piacra lépés előtt
        if self.stats.total_pnl <= -config.risk_management.max_daily_loss_usd:
            print(f"🛑 Napi veszteség limit eléréve (-${-self.stats.total_pnl:.2f}). Bot leáll.")
            self.state = BotState.HALT
            self.cooldown_until = time.time() + 86400 # 24 óra
            return
            
        # Hard Guardrail: Több aktív rendeléssel nem kockáztatunk a jelenlegi pipeline-al
        # (Jelenleg 1 létrával operálunk egyszerre, de future-proofoljuk)
        if self.order_mgr.active_ladder is not None:
             print("⚠️ Hiba: Már van lógó ladder.")
             self.state = BotState.LADDER_PLACED
             return
        
        # Log BTC price every 5 seconds
        now = time.time()
        if now - self.last_price_log > 5.0:
            btc_price = self.binance.get_current_price()
            if btc_price:
                change = ""
                if self.last_logged_price:
                    pct = ((btc_price - self.last_logged_price) / self.last_logged_price) * 100
                    change = f" ({pct:+.3f}%)"
                
                print(f"📊 BTC: ${btc_price:,.2f}{change} | Monitoring...")
                self.last_logged_price = btc_price
            self.last_price_log = now
        
        signal = self.signal_engine.update()
        
        if signal:
            self.last_signal_time = time.perf_counter()
            self.stats.signals_detected += 1
            print(f"\n🚨 SIGNAL #{self.stats.signals_detected}")
            print(f"   Direction: {signal.direction}")
            print(f"   Change: {signal.pct_change:+.3f}%")
            print(f"   Duration: {signal.duration_ms:.0f}ms")
            print(f"   Price: ${signal.start_price:,.2f} → ${signal.end_price:,.2f}")
            
            # Map to token
            if signal.direction == "BEARISH":
                token_id = self.market.down_token_id if self.market else None
                side = "DOWN"
            else:
                token_id = self.market.up_token_id if self.market else None
                side = "UP"
            
            if not token_id:
                print("   ⚠️  No market available, skipping")
                return
            
            # Place ladder
            print(f"   → Placing {side} ladder...")
            
            # Fetch real yes/no token price depending on side
            if side == "UP":
                mid_price = self.poly_client.get_yes_price(token_id) or 0.50
            else:
                mid_price = self.poly_client.get_yes_price(token_id) or 0.50 # On PM, UP/DOWN token has its own "Yes" price usually
                
            # Dinamikus position sizing a konfigból
            max_usd = config.risk_management.max_notional_usd_per_trade
            total_shares = max(config.min_shares,
                               int(max_usd / mid_price))

            ladder = self.order_mgr.place_ladder(
                token_id=token_id,
                side=side,
                mid_price=mid_price,
                total_shares=total_shares,
                tick_size=self.market.tick_size,
                inventory_yes=self.inventory_yes,
                inventory_no=self.inventory_no
            )
            self.last_quote_time = time.perf_counter()
            
            if ladder:
                self.stats.ladders_placed += 1
                self.state = BotState.LADDER_PLACED
                print(f"   ✅ Ladder placed")
    
    def _ladder_placed_tick(self):
        """Just placed ladder - transition to waiting"""
        self.state = BotState.WAITING_FILL
    
    def _waiting_fill_tick(self):
        """Waiting for fills"""
       
        # Check ladder age
        age_ms = self.order_mgr.get_ladder_age_ms()
        
        any_filled = False
        filled_size = 0.0
        avg_price = 0.0
        
        if self.dry_run and self.order_mgr.active_ladder:
            # SHADOW RUN LOGIC: Fiktív soft-fill ellenőrzés
            token = self.order_mgr.active_ladder.token_id
            hyp_price = self.order_mgr.active_ladder.orders[0].price

            # Guard: csak ha van élő kliens
            has_client = (self.poly_client
                          and hasattr(self.poly_client, 'client')
                          and self.poly_client.client is not None)
            if has_client:
                try:
                    book = self.poly_client.client.get_order_book(token)
                    if book and book.bids and book.asks:
                        best_ask = float(book.asks[0].price)
                        if best_ask <= hyp_price:
                            any_filled = True
                            filled_size = self.order_mgr.active_ladder.orders[0].size
                            avg_price = best_ask
                            print(f"👻 [SHADOW] Soft-Fill: {best_ask} <= {hyp_price}")
                except Exception as e:
                    print(f"⚠️ Shadow book fetch error: {e}")
            else:
                # Nincs kliens → REST fallback a soft-fillhez
                try:
                    ask = self.poly_client.get_ask_price(token) if self.poly_client else None
                    if ask and ask <= hyp_price:
                        any_filled = True
                        filled_size = self.order_mgr.active_ladder.orders[0].size
                        avg_price = ask
                        print(f"👻 [SHADOW] Soft-Fill (REST): {ask} <= {hyp_price}")
                except Exception as e:
                    print(f"⚠️ Shadow REST fetch error: {e}")
        else:
            # Check for real fills
            any_filled, filled_size, avg_price = self.order_mgr.check_fills()
        
        if any_filled:
            # Transition to position
            self.stats.fills_received += 1
            print(f"\n✅ FILLED: {filled_size} shares @ ${avg_price:.2f}")
            
            # Toxicity Monitor beküldése
            # Az időszinkron fontos: wall-clock a logginghoz, perf_counter a latencyhez
            self.toxicity_engine.register_fill(
                token_id=self.order_mgr.active_ladder.token_id,
                side=self.order_mgr.active_ladder.side,
                price=avg_price,
                size=filled_size,
                t_signal=self.last_signal_time,
                t_quote_sent=self.last_quote_time,
                wall_clock_time=time.time()
            )
            
            # Update inventory logic
            active_side = self.order_mgr.active_ladder.side
            if active_side == "UP":
                self.inventory_yes += filled_size
            else:
                self.inventory_no += filled_size
                
            # Másodpercek számítása GTD Time Stop-hoz (Time-to-Resolution)
            time_to_res_sec = int(
                (self.market.end - datetime.datetime.now(
                    datetime.timezone.utc)).total_seconds())

            # Place TP (dinamikusan a toxicity engine score alapján is húzható kicsit közelebb)
            current_tox = self.toxicity_engine.toxicity_score
            self.exit_mgr.place_take_profit(
                token_id=self.order_mgr.active_ladder.token_id,
                entry_price=avg_price,
                position_size=filled_size,
                current_spread=self._get_current_spread() or 0.04,
                tick_size=self.market.tick_size,
                time_to_resolution_sec=time_to_res_sec,
                toxicity_score=current_tox
            )
            
            # Cancel unfilled ladder orders
            # (order_mgr handles this automatically)
            
            self.state = BotState.IN_POSITION
        
        elif age_ms > 1500:  # 1.5 second timeout
            # No fills - cancel and skip
            print(f"\n⏱️  No fills after 1.5s - canceling (no edge)")
            self.order_mgr.cancel_ladder()
            self.stats.no_fill_skips += 1
            self._enter_cooldown()
    
    def _in_position_tick(self):
        """In position - check exit conditions"""
        
        # Check exit conditions for the position
        should_exit, reason = self.exit_mgr.check_exit_conditions()
        
        if should_exit:
            print(f"\n📤 EXITING: {reason}")
            self.state = BotState.EXITING
            self._exit_position(reason)
    
    def _exit_position(self, reason: str):
        """Exit position - cancel TP és ifőkorlát esetén market sell."""

        # Cancel pending exit orders (TP)
        self.exit_mgr.cancel_exit_orders()

        # Ha TIME_STOP_FALLBACK vagy más kényszer exit,
        # küldjünk tényleges SELL ordert LIVE módban
        if not self.dry_run and self.order_mgr.active_ladder:
            token_id = self.order_mgr.active_ladder.token_id
            filled = self.order_mgr.active_ladder.total_size_filled
            if filled > 0:
                try:
                    from py_clob_client.order_builder.constants import SELL
                    sell_order = {
                        "token_id": token_id,
                        "price": 0.01,  # Market sell (leg low limit)
                        "size": filled,
                        "side": SELL,
                        "expiration": 5  # Gyors GTD
                    }
                    resp = self.poly_client.place_batch_orders(
                        [sell_order], dry_run=False
                    )
                    if resp:
                        print(f"   💰 SELL order küldve: "
                              f"{filled} shares")
                    else:
                        print("   ⚠️ SELL order sikertelen!")
                except Exception as e:
                    print(f"   ❌ SELL order hiba: {e}")
        elif self.dry_run and self.order_mgr.active_ladder:
            filled = self.order_mgr.active_ladder.total_size_filled
            print(f"   🧪 [DRY] Szimulált SELL: {filled} shares")

        # Log exit
        self.stats.exits_completed += 1
        print(f"   ✅ Exit #{self.stats.exits_completed} ({reason})")

        # Enter cooldown
        self._enter_cooldown()
    
    def _get_current_spread(self) -> Optional[float]:
        """Fetch live spread from orderbook (best_ask - best_bid)"""
        if not self.market or not self.poly_client:
            return None
        has_client = (hasattr(self.poly_client, 'client')
                      and self.poly_client.client is not None)
        if not has_client:
            return None
        try:
            token = self.market.up_token_id
            book = self.poly_client.client.get_order_book(token)
            if book and book.bids and book.asks:
                return float(book.asks[0].price) - float(book.bids[0].price)
        except Exception:
            pass
        return None

    def _enter_cooldown(self):
        """Enter cooldown state"""
        # FIGYELEM: inventory nullázás CSAK DRY RUN módban biztonságos.
        # LIVE módban a tényleges pozíciót a blockchain állapotból kellene olvasni.
        if self.dry_run:
            self.inventory_yes = 0
            self.inventory_no = 0
        else:
            # TODO: Blockchain inventory lekérdezés
            print("   ⚠️ LIVE: Inventory NEM nullázva (blockchain state kell)")

        cooldown_sec = config.risk_management.cooldown_after_exit_sec
        self.cooldown_until = time.time() + cooldown_sec
        self.state = BotState.COOLDOWN
        print(f"   ⏸️  Cooldown for {cooldown_sec}s")
    
    def _cooldown_tick(self):
        """Cooldown state"""
        if time.time() >= self.cooldown_until:
            print(f"\n✅ Cooldown complete - back to ARMED")
            self.state = BotState.ARMED
    
    def _shutdown(self):
        """Clean shutdown"""
        if self.binance:
            self.binance.stop()
        
        print()
        print("=" * 60)
        print("📊 SESSION STATS")
        print("=" * 60)
        print(f"Signals detected: {self.stats.signals_detected}")
        print(f"Ladders placed: {self.stats.ladders_placed}")
        print(f"Fills received: {self.stats.fills_received}")
        print(f"No-fill skips: {self.stats.no_fill_skips}")
        print(f"Exits completed: {self.stats.exits_completed}")
        print(f"Total P&L: ${self.stats.total_pnl:.2f}")
        print()
        print("✅ Bot stopped cleanly")


if __name__ == "__main__":
    import argparse
    
    parser = argparse.ArgumentParser(description='Crypto Sebesseg Trading Bot')
    parser.add_argument('--live', action='store_true', help='Run in LIVE mode (default: DRY RUN)')
    args = parser.parse_args()
    
    dry_run = not args.live
    
    if not dry_run:
        print("⚠️  WARNING: LIVE MODE!")
        print("   Real orders will be placed!")
        response = input("   Type 'YES' to confirm: ")
        if response != "YES":
            print("   Aborted")
            sys.exit(0)
    
    bot = TradingBot(dry_run=dry_run)
    bot.run()
