# pyre-ignore-all-errors
import os
from typing import Optional, Dict, Any, List, Callable
from bot_logger import logger
from dotenv import load_dotenv

from eth_account.signers.local import LocalAccount
from eth_account import Account

from hyperliquid.utils import constants
from hyperliquid.info import Info
from hyperliquid.exchange import Exchange

load_dotenv()

class HyperliquidClient:
    """
    Hyperliquid API Kliens (Official Python SDK Wrapper)
    """
    
    def __init__(self, dry_run: bool = True, user_events_callback: Optional[Callable[[Dict[str, Any]], None]] = None):
        self.dry_run = dry_run
        self.user_events_callback = user_events_callback
        
        # Keys
        self.wallet: Optional[LocalAccount] = None
        priv_key = os.getenv("PRIVATE_KEY")
        if not priv_key or "ide_" in priv_key:
            logger.info("⚠️ FIGYELEM: PRIVATE_KEY nincs beállítva az .env-ben!")
        else:
            try:
                wallet = Account.from_key(priv_key)
                self.wallet = wallet
                logger.info(f"✅ HL Wallet betöltve: {wallet.address[:6]}...{wallet.address[-4:]}")
            except Exception as e:
                logger.info(f"❌ Privát kulcs betöltési hiba: {e}")
        
        # SDK instances (Testnet for dry_run? Or Mainnet without sending orders?)
        # Let's stick to mainnet but simply not call exchange.order if dry_run.
        self.base_url = constants.MAINNET_API_URL
        # Exchange only needs wallet if we sign
        # Note: If we need WebSocket userEvents, we must set skip_ws=False
        skip_ws = self.user_events_callback is None
        self.info = Info(self.base_url, skip_ws=skip_ws)
        self.exchange = Exchange(self.wallet, self.base_url) if self.wallet else None
        
        # Subscribe to userEvents if requested
        w = self.wallet
        if not skip_ws and w is not None and self.user_events_callback:
            logger.info("📡 Csatlakozás a HL WebSocket 'userEvents' csatornához...")
            self.info.subscribe(
                {"type": "userEvents", "user": w.address},
                self.user_events_callback
            )
        
        if getattr(self, "base_url", None) is None:
            self.base_url = constants.MAINNET_API_URL
        
        self.metaCache = self.info.meta()
        self.coin_to_idx: Dict[str, int] = {}
        for idx, coin_data in enumerate(self.metaCache.get("universe", [])):
            self.coin_to_idx[coin_data.get("name")] = idx
            
        logger.info(f"🔗 HL SDK Meta adatok betöltve. Összes piac: {len(self.coin_to_idx)}")

    # ================= INFO API =================
    
    def get_mid_price(self, coin: str) -> Optional[float]:
        try:
            return self.info.l2_snapshot(coin)
        except Exception:
            # Info returned object has no simple interface for mid price easily without ws? We use L2Snapshot from info.py
            pass
        
        # Fallback to direct REST
        import requests
        resp = requests.post(f"{self.base_url}/info", json={"type": "l2Book", "coin": coin})
        if resp.status_code == 200:
            book = resp.json()
            levels = book.get("levels", [])
            if len(levels) == 2 and len(levels[0]) > 0 and len(levels[1]) > 0:
                best_bid = float(levels[0][0]["px"])
                best_ask = float(levels[1][0]["px"])
                return (best_bid + best_ask) / 2.0
        return None

    def get_user_state(self) -> Optional[Dict[str, Any]]:
        w = self.wallet
        if w is None:
            return None
        try:
            return self.info.user_state(w.address)
        except Exception as e:
            logger.info(f"❌ User state lekérdezési hiba: {e}")
            return None

    def get_account_value(self) -> float:
        """
        Retrieves the total account value (margin + unrealized PnL) from the Hyperliquid API.
        Returns 0.0 if not available.
        """
        state = self.get_user_state()
        if not state:
            return 0.0
        try:
            # Margin summary contains total margin value
            return float(state.get("marginSummary", {}).get("accountValue", 0.0))
        except Exception as e:
            logger.info(f"❌ Account value feldolgozási hiba: {e}")
            return 0.0

    def update_leverage(self, coin: str, leverage: int, is_cross: bool = True) -> bool:
        """Beállítja a tőkeáttételt a megadott érmére."""
        if self.dry_run:
            logger.info(f"🧪 [DRY RUN] Tőkeáttétel szimulálva: {coin} - {leverage}x {'Cross' if is_cross else 'Isolated'}")
            return True
            
        ex = self.exchange
        if ex is None:
            logger.info("❌ Nincs SDK Exchange példányosítva.")
            return False
        
        if coin not in self.coin_to_idx:
            logger.info(f"❌ Ismeretlen HL coin: {coin}.")
            return False
            
        logger.info(f"🔗 HL Leverage beállítása: {coin} -> {leverage}x {'Cross' if is_cross else 'Isolated'}")
        
        try:
            res = ex.update_leverage(leverage, coin, is_cross)
            logger.info(f"   Lev. frissítve: {res}")
            return True
        except Exception as e:
            logger.info(f"❌ Lev. beállítás hiba: {e}")
            return False

    # ================= EXCHANGE API =================
    
    def cancel_all_orders(self, coin_filter: Optional[str] = None) -> bool:
        """Mindent azonnal töröl."""
        if self.dry_run:
            logger.info(f"🧪 [DRY RUN] BATCH CANCEL ALL végrehajtva. ({coin_filter or 'MINDEN'})")
            return True
            
        ex = self.exchange
        w = self.wallet
        if ex is None or w is None:
            return False
            
        try:
            open_orders = self.info.open_orders(w.address)
            cancels = []
            for o in open_orders:
                if coin_filter and o["coin"] != coin_filter:
                    continue
                cancels.append({"coin": o["coin"], "o": o["oid"]})
            
            if not cancels:
                return True
                
            res = ex.cancel(cancels)
            logger.info(f"🗑️ HL Törlés ({len(cancels)} order): {res}")
            return True
        except Exception as e:
            logger.info(f"❌ Cancel all error: {e}")
            return False

    def get_open_orders(self) -> List[Dict[str, Any]]:
        """Visszaadja a wallet összes nyitott orderét a HL API-ból."""
        w = self.wallet
        if w is None:
            return []
        try:
            return self.info.open_orders(w.address)
        except Exception as e:
            logger.warning(f"❌ get_open_orders hiba: {e}")
            return []
