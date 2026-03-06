# pyre-ignore-all-errors
import os
import time
import asyncio
import argparse
from bot_logger import logger
from typing import Optional, Dict, Any

from config import config
from hyperliquid_client import HyperliquidClient
from hyperliquid_feed import HyperliquidFeed
from signal_engine import SignalEngine
from order_manager import OrderManager, ExitManager

class SebessegBot:
    def __init__(self, dry_run: bool = True):
        self.dry_run = dry_run
        
        # Állapotgépi Változók
        self.state = "IDLE"
        self.active_coin: Optional[str] = "BTC" # Default coin to trade
        
        # Risk Management - Hyperliquid Perpetual specific (LONG / SHORT margin)
        self.daily_pnl = 0.0
        self.consecutive_losses = 0
        
        # Kliensek & Engine-ek
        self.hl_client: Any = None
        self.feed_engine: Any = None
        self.signal_engine: Any = None
        self.order_manager: Any = None
        self.exit_manager: Any = None
        
        self.cooldown_end_time: float = 0.0
        self.last_trade_timestamp: float = 0.0
        self.last_tick_time = 0.0
        self.min_tick_interval = 0.01  # 10ms for lower loop stress but fast reaction
        self._last_stale_warn: float = 0.0   # log throttle: max 1 stale warning / 5s
        
        self.trade_params = {
            "current_mid": 0.0,
            "target_side": "",
            "sz_usd": 0.0,
            "tick_size": 1.0, # Ezt lekérjük config/API-ből
        }
        
    def initialize(self) -> bool:
        logger.info(f"🚀 INICIALIZÁLÁS: SebessegBot v2 (Hyperliquid) | DRY_RUN={self.dry_run}")
        
        if config.strategy_name != "pm_ambush_ladder_maker_v2":
            logger.warning(f"Strategy name mismatch! Found: {config.strategy_name}")
            
        # 1. HL Client
        self.hl_client = HyperliquidClient(dry_run=self.dry_run)
        
        # Cancel any leftover open orders from previous crash
        if not self.dry_run:
            self.hl_client.cancel_all_orders()

        # Update Leverage
        lev_cfg = config.risk_management.leverage
        self.hl_client.update_leverage(
            coin=self.active_coin,
            leverage=lev_cfg.max_leverage,
            is_cross=lev_cfg.cross_margin
        )
        
        # Fetch initial precise tick_size / lot size requirements from info
        meta = self.hl_client.metaCache
        if meta and "universe" in meta:
            try:
                coin_idx = self.hl_client.coin_to_idx[self.active_coin]
                coin_data = meta["universe"][coin_idx]
                sz_decimals = int(coin_data.get("szDecimals", 4))
                # Minimum tick_size usually hardcoded but can be pulled
            except Exception as e:
                logger.info(f"Could not fetch metadata sizing: {e}")
                
        # We enforce a dynamic tick size for testing
        self.trade_params["tick_size"] = 1.0 if self.active_coin == "BTC" else 0.01

        # 2. Market Feed (L2 WebSocket)
        self.feed_engine = HyperliquidFeed(coin=self.active_coin)
        self.feed_engine.start()
        
        # Várakozás az első adatokra
        logger.info("⏳ Várakozás a Hyperliquid Feed stabilizálódására...")
        time.sleep(3)
        if not self.feed_engine.get_current_price() or self.feed_engine.get_current_price() <= 0:
            logger.info("❌ Hiba: Nincs kezdeti Mid Price az L2 WebSocket bookból!")
            return False
        logger.info(f"✅ Kezdeti Vola / Price feed betöltve. Ár: ${self.feed_engine.get_current_price():.2f}")

        # 3. Signal Engine
        self.signal_engine = SignalEngine(self.feed_engine)
        
        # 4. Order Managers
        self.order_manager = OrderManager(self.hl_client, dry_run=self.dry_run)
        self.exit_manager = ExitManager(self.hl_client, dry_run=self.dry_run)
        
        # Start state
        self.state = "ARMED"
        return True

    async def run_async(self, stop_event: asyncio.Event):
        """Aszinkron fő ciklus – az asyncio event loop tartja életben a háttérszálakat"""
        RUNNING_STATES = {"ARMED", "LADDER_PLACED", "IN_POSITION",
                          "EXITING", "COOLDOWN", "RECOVERING"}

        logger.info("🟢 BOT INDÍTÁSA - Aszinkron hurok fut... (leállítás: Ctrl+C vagy systemctl stop)")

        try:
            while self.state in RUNNING_STATES and not stop_event.is_set():
                now = time.time()
                if now - self.last_tick_time < self.min_tick_interval:
                    await asyncio.sleep(0.001)  # ← async sleep: nem blokkolja az event loopot!
                    continue
                self.last_tick_time = now

                # Feed egészség – kétlépcsős staléness védelem
                if self._check_feed_health():
                    await asyncio.sleep(0.01)
                    continue

                # --- State Machine ---
                if self.state == "ARMED":
                    self._armed_tick()
                elif self.state == "LADDER_PLACED":
                    self._ladder_placed_tick()
                elif self.state == "IN_POSITION":
                    self._in_position_tick()
                elif self.state == "EXITING":
                    self._exiting_tick()
                elif self.state == "COOLDOWN":
                    self._cooldown_tick()
                elif self.state == "RECOVERING":
                    self._recovering_tick()

        except Exception as e:
            logger.error(f"💥 KRITÍKUS HIBA: {e}", exc_info=True)
        finally:
            # Garantált takarítás
            self.shutdown()


    def _check_feed_health(self) -> bool:
        """Ellenőrzi a WebSocket L2 Book kapcsolat él-e még. Kétlépcsős védelem."""
        if not self.feed_engine:
            return False
            
        staleness = self.feed_engine.get_staleness_sec()
        
        # 2. lépcső: Panic Cancel & Pozíció zárása (3 másodperc felett)
        if staleness > 3.0:
            if self.state == "RECOVERING":
                return True # Már pánikoltunk és recoveryben vagyunk, ne spammeljük az API-t

            logger.error(f"🚨 KRITIKUS FEED STALE ({staleness:.1f}s): Vakság! Panic Cancel & Market Close!")
            if not self.dry_run and self.hl_client:
                self.hl_client.cancel_all_orders()
            if self.order_manager:
                self.order_manager.cancel_ladder()
            if self.exit_manager:
                logger.error("   Azonnali Piaci Zárás indítása a beragadás elkerülésére...")
                self.exit_manager.close_position_at_market()
                
            self.state = "RECOVERING"
            return True
            
        # 1. lépcső: Warning (1.5-3 másodperc között)
        # 1.0s → 1.5s: Tokió VPS-en normál csúcslatencia 1.1-1.3s lehet
        # Log throttle: max 1 figyelmeztetés / 5 másodperc (elnémítja a spam-et)
        if staleness > 1.5:
            now_w = time.time()
            if self.state == "ARMED":
                if now_w - self._last_stale_warn >= 5.0:
                    logger.warning(f"⚠️ FEED STALE WARNING ({staleness:.1f}s): Nincs új belépés, várakozás...")
                    self._last_stale_warn = now_w
                return True  # Skip the tick, prevent new orders, but don't halt
            else:
                # In LADDER_PLACED or IN_POSITION

                if now_w - self._last_stale_warn >= 5.0:
                    logger.warning(f"⚠️ FEED STALE WARNING ({staleness:.1f}s): Létra/Pozíció aktív, várakozás a helyreállásra mielőtt piacit zárunk...")
                    self._last_stale_warn = now_w
                return False
                
        return False

    # ================= ÁLLAPOT CIKLUSOK =================
    
    def _armed_tick(self):
        """Jelzések figyelése és Ladder elhelyezése"""
        signal, metadata = self.signal_engine.update()  # mindig Tuple[Optional[str], dict]

        if not signal:
            return

        logger.info(f"⚡ JELZÉS ÉRKEZETT: {signal} (Z-Score: {metadata.get('z_score', 0):.4f})")

        # Toxic Flow Check!
        v_pct = metadata.get("velocity_pct_sec", 0)
        dur = metadata.get("duration_ms", 0)
        if config.is_toxic_flow(v_pct, dur):
            logger.info(f"🛡️ TOXIKUS KLIMA SZŰRVE: Sebesség={v_pct:.3f}%/s, Idő={dur}ms. SKIP!")
            return
            
        # Daily Loss limit check
        if self.daily_pnl <= -float(abs(config.risk_management.max_daily_loss_usd)):
            logger.info(f"🚨 DAILY LOSS LIMIT ELÉRVE (${self.daily_pnl:.2f}). HALT!")
            self.state = "HALT"
            return
            
        # Decision Table for HL 
        # Bullish -> Panic buy momentum -> we place SHORT trap above (mean reversion)
        # Bearish -> Panic sell momentum -> we place LONG trap below
        side = "SHORT" if signal == "BULLISH" else "LONG"
        mid_price = self.feed_engine.get_current_price()
        
        # Max Size Limit
        target_usd = config.risk_management.max_notional_usd_per_trade
        
        logger.info(f"🎯 LÉTRA INDÍTÁSA:")
        logger.info(f"   Irány: {side} {self.active_coin}")
        logger.info(f"   Mid Ár: ${mid_price:.2f}")
        logger.info(f"   Szándékolt Méret: ${target_usd:.2f} Notional")

        # PILLANATNYI ADATOK MENTÉSE (A későbbi állapotokhoz)
        self.trade_params["current_mid"] = mid_price
        self.trade_params["target_side"] = side
        self.trade_params["sz_usd"] = target_usd
        
        ladder = self.order_manager.place_ladder(
            coin=self.active_coin,
            side=side,
            mid_price=mid_price,
            total_usd_notional=target_usd,
            tick_size=self.trade_params["tick_size"]
        )
        
        if ladder:
            # Check for ALO rejections
            valid_orders = []
            if not self.dry_run:
                for o in ladder.orders:
                    if o.order_id and not o.order_id.startswith("ERR_") and not o.order_id.startswith("ALO_REJECT_"):
                        valid_orders.append(o)
                
                if not valid_orders:
                    logger.info("❌ Minden létrafok ALO_REJECT / ERR miatt visszadobva! Vissza ARMED-be.")
                    self.order_manager.cancel_ladder()
                    self.state = "ARMED"
                    return
                    
            self.state = "LADDER_PLACED"
        else:
            logger.info("❌ Létra elhelyezése SIKERTELEN. Vissza ARMED állapotba.")

    def _ladder_placed_tick(self):
        """Várakozás a részleges / teljes fill-re"""
        
        has_fills, filled_size, avg_price = self.order_manager.check_fills()
        
        age_ms = self.order_manager.get_ladder_age_ms()
        if age_ms > config.wait_for_fill_ms:
            # IDŐTÚLLÉPÉS
            if has_fills:
                logger.info(f"⏳ LÉTRA IDŐTÚLLÉPÉS, de VOLT RÉSZLEGES FILL. Törlés és EXITING ciklus.")
                self.order_manager.cancel_ladder()
                # Pass data to exit manager
                self.state = "EXITING"
                # Call immediately to setup TP
                self._setup_take_profit(filled_size, avg_price)
            else:
                logger.info(f"⏳ LÉTRA IDŐTÚLLÉPÉS ({age_ms:.0f}ms). NO EDGE SKIP. Törlés.")
                self.order_manager.cancel_ladder()
                self.state = "ARMED"  # Vissza vadászni
            return

        # Ha MINDEN kitöltődött a MAX TTR elérése előtt...
        # HL-nél lehet gyors is.
        if has_fills:
            # OCO Behavior: on_any_ladder_fill = cancel_all_other
            # Strategy requests to cancel unfilled!
            if config._config['order_management']['oco_behavior']['on_any_ladder_fill'] == "cancel_all_other_ladder_levels":
                logger.info("⚡ OCO TRIGGERED: Canceled resting levels because we got a fill!")
                self.order_manager.cancel_ladder()
                self._setup_take_profit(filled_size, avg_price)
                self.state = "IN_POSITION"
            

    def _setup_take_profit(self, position_size: float, avg_price: float):
        # 1 lépés: Spread kinyerése
        spread = 0.0 # TODO: Feed-ben most nincs spread proxy, fix tick size * x
        
        # Optional: Toxicity
        toxicity = 0.0 # self.signal_engine.last_sig_score ha lenne
        
        logger.info(f"📈 TAKE PROFIT SETUP: {self.trade_params['target_side']} @ ${avg_price:.2f} ({position_size} shares)")
        
        placed = self.exit_manager.place_take_profit(
            coin=self.active_coin,
            side=self.trade_params['target_side'],
            entry_price=avg_price,
            position_size=position_size,
            current_spread=spread,
            tick_size=self.trade_params['tick_size'],
            toxicity_score=toxicity
        )
        
        if placed:
            self.state = "IN_POSITION"
        else:
            logger.info("❌ TP ELHELYEZÉSE SIKERTELEN. Market Close Action...")
            # Ideally Market Close, falling back to Cooldown
            self.state = "COOLDOWN"

    def _in_position_tick(self):
        """Várakozás Take Profitra vagy Time Stopra"""
        # Time stop check!
        time_to_close, reason = self.exit_manager.check_exit_conditions()
        
        # Check API if TP is filled
        if not self.dry_run:
            pass # TODO: call info.open_orders and see if TP exists
            
        if self.dry_run:
            # Simulate FAST fill
            time.sleep(2)
            time_to_close = True
            reason = "DRY_RUN_SIMULATION_TP_REACHED"
        
        if time_to_close:
            logger.info(f"🚪 EXIT KIVÁLTVA: {reason}")
            
            if reason == "TAKE_PROFIT_REACHED":
                logger.info("✅ Sikeres TP kilépés a hálózaton!")
                self.daily_pnl += 5.0 # Dummy
                self.exit_manager._reset_state()
            else:
                logger.info("⚠️ Hálózati Market Close kikényszerítése...")
                self.exit_manager.close_position_at_market()
            
            self._start_cooldown(reason)
            

    def _exiting_tick(self):
        """
        Abban az esetben ha cancel-özünk pozíciót piacin (Market close)
        """
        # Not fully implemented without real market orders on HL yet
        self.state = "COOLDOWN"
        self.cooldown_end_time = time.time() + config.risk_management.cooldown_after_exit_sec


    def _start_cooldown(self, action: str, override_sec: Optional[int] = None):
        if override_sec is not None:
            sec = override_sec
        else:
            sec = config.risk_management.cooldown_after_exit_sec
            if "STOP_LOSS" in action:
                sec = 120 # longer wait 
            
        logger.info(f"🧊 COOLDOWN AKTIVÁLVA ({sec} mp). Ok: {action}")
        self.cooldown_end_time = time.time() + sec
        self.state = "COOLDOWN"


    def _cooldown_tick(self):
        """Zárolás lejárásának figyelése"""
        if time.time() >= self.cooldown_end_time:
            logger.info("🔥 COOLDOWN LEJÁRT. Vissza a harcba!")
            self.state = "ARMED"
            # Signal Engine-ben debouncer bypass vagy reset
            self.signal_engine.last_trade_timestamp = time.time()

    def _recovering_tick(self):
        """Hálózati szakadás utáni talpraállás és ellenőrzés"""
        if self.feed_engine.get_staleness_sec() > 1.0:
            # Még mindig szakadt a vonal
            time.sleep(1.0)
            return
            
        logger.info("📡 Feed újra él! Pozíciók és aktív megbízások ellenőrzése (Recovery Mode)...")
        
        if self.dry_run:
            logger.info("🧪 [DRY RUN] Recovery sikeres. Átállás 30s COOLDOWN-ra...")
            self._start_cooldown("RECOVERY_CLEAR", override_sec=30)
            return
            
        is_clear = True
        try:
            # a) Nincs nyitott megbízás
            open_orders = self.hl_client.info.open_orders(self.hl_client.wallet.address)
            if open_orders:
                logger.warning(f"⚠️ Recovery: Találtam {len(open_orders)} árva megbízást! Törlés...")
                self.hl_client.cancel_all_orders()
                is_clear = False
                
            # b) Nincs nyitott pozíció
            state = self.hl_client.info.user_state(self.hl_client.wallet.address)
            positions = state.get("assetPositions", [])
            for pos in positions:
                if pos["position"]["coin"] == self.active_coin:
                    sz = float(pos["position"]["szi"])
                    if abs(sz) > 0:
                        logger.warning(f"⚠️ Recovery: Beragadt pozíciót találtam ({sz})! Market Close kikényszerítése...")
                        self.exit_manager.coin = self.active_coin
                        self.exit_manager.close_position_at_market()
                        is_clear = False
                        break
        except Exception as e:
            logger.error(f"❌ Recovery API ellenőrzés hiba: {e}")
            is_clear = False
            
        if is_clear:
            logger.info("✅ Minden tiszta! Bot visszatér a normál működéshez 30 másodperc múlva.")
            self._start_cooldown("RECOVERY_CLEAR", override_sec=30)
        else:
            # Ha nem tiszta, visszatartjuk és kövi tickben próbáljuk megint
            time.sleep(2.0)

    def shutdown(self):
        """Biztonságos leállítás garantálása!"""
        logger.info("⚠️ BIZTONSÁGI LEÁLLÍTÁS (SHUTDOWN) FOLYAMATA...")
        self.state = "HALT"
        
        if self.feed_engine:
            self.feed_engine.stop()
            
        if self.order_manager:
            self.order_manager.cancel_ladder()
            
        if self.exit_manager:
            self.exit_manager.cancel_exit_orders()
            
        if self.hl_client and not self.dry_run:
            self.hl_client.cancel_all_orders()
            
        logger.info("✅ LEÁLLÍTÁS SIKERES. Good Night!")

async def _bot_shutdown(bot: SebessegBot, stop_event: asyncio.Event) -> None:
    """Aszinkron leállítási jel kezelése (SIGTERM / SIGINT)"""
    logger.warning("📨 Leállítási jelérkezett. Takarítás folyamatban...")
    bot.state = "HALT"
    stop_event.set()


async def main_async(is_live: bool) -> None:
    """
    Aszinkron fő belépési pont. Az asyncio event loop tartja életben
    a háttérszálakat (WebSocket asyncio loop), így nem zavarodik
    össze a leállításkor.
    """
    import signal as _signal

    bot = SebessegBot(dry_run=not is_live)

    if not bot.initialize():
        logger.error("❌ INICIALIZÁLÁS SIKERTELEN.")
        raise SystemExit(1)

    stop_event = asyncio.Event()

    # Aszinkron jel kezelés – ez a helyes mód asyncio programban!
    # (A szinkron signal.signal nem játszik jól az event looppal)
    loop = asyncio.get_running_loop()
    for sig in (_signal.SIGINT, _signal.SIGTERM):
        loop.add_signal_handler(
            sig,
            lambda: asyncio.ensure_future(_bot_shutdown(bot, stop_event))
        )

    await bot.run_async(stop_event)


if __name__ == "__main__":
    import signal  # noqa
    parser = argparse.ArgumentParser(description="Sebesseg Crypto Maker Bot")
    parser.add_argument('--live', action='store_true', help='ÉLES KERESKEDÉS (pénzt kockáztatsz!)')
    args = parser.parse_args()

    if not args.live:
        print("🧪 DRY RUN mód aktiválva. Valós orderek NEM kerülnek kiállításra.")
    else:
        print("🚨 LIVE mód! Valódi tőke kockán!")
        print("   Ctrl+C = leállítás. 3 másodperc múlva indul...")
        time.sleep(3)

    asyncio.run(main_async(args.live))
