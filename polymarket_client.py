from bot_logger import logger
"""
Polymarket kliens - Itt történnek a tényleges fogadások
Használja a py-clob-client könyvtárat
"""

import os
import asyncio
import requests
from dotenv import load_dotenv

# Betölti a .env fájlból a kulcsokat
load_dotenv()
from config import config

# Próbáljuk importálni a py-clob-client-et
try:
    from py_clob_client.client import ClobClient
    from py_clob_client.clob_types import OrderArgs
    from py_clob_client.order_builder.constants import BUY, SELL
    CLOB_AVAILABLE = True
except ImportError:
    logger.info("⚠️  py-clob-client nincs telepítve. Futtasd: pip install py-clob-client")
    CLOB_AVAILABLE = False


class PolymarketClient:
    def __init__(self):
        self.private_key = os.getenv("PRIVATE_KEY", "")
        self.funder_address = os.getenv("FUNDER_ADDRESS", "") # NEW: For Proxy Wallet
        self.host = "https://clob.polymarket.com"
        self.chain_id = 137  # Polygon mainnet
        
        if not self.private_key or "ide_" in self.private_key:
            logger.info("⚠️  FIGYELEM: Polymarket PRIVATE_KEY nincs beállítva!")
            logger.info("   Szerkeszd a .env fájlt és add meg a privát kulcsot.")
            self.client = None
            self.creds = None
        elif not self.funder_address:
            logger.info("⚠️  FIGYELEM: Csatlakozás visszautasítva: FUNDER_ADDRESS (Proxy) hiányzik.")
            self.client = None
            self.creds = None
        elif not CLOB_AVAILABLE:
            logger.info("⚠️  py-clob-client könyvtár hiányzik!")
            self.client = None
            self.creds = None
        else:
            logger.info("✅ Polymarket privát kulcs és Proxy cím betöltve")
            # A klienst majd aszinkron módon inicializáljuk
            self.client = None
            self.creds = None
        
        # Persistent session for high-speed reads
        self.session = requests.Session()
        self.session.headers.update({
            "User-Agent": "Mozilla/5.0",
            "Accept": "application/json"
        })
    
    def initialize(self):
        """Inicializálás (Sync) - API kulcsok generálása Proxy-n keresztül"""
        if not CLOB_AVAILABLE or not self.private_key or not self.funder_address:
            return False
        
        try:
            # Ideiglenes kliens az API kulcsok lekéréséhez a proxy címről
            temp_client = ClobClient(
                self.host, 
                key=self.private_key, 
                chain_id=self.chain_id,
                funder=self.funder_address,
                signature_type=1  # Polymarket Proxy (CTF Exchange)
            )
            
            # API kulcsok generálása/lekérése (a Proxy aláírásával delegáljuk)
            self.creds = temp_client.create_or_derive_api_creds()
            
            # Végleges kliens inicializálása
            self.client = ClobClient(
                self.host,
                key=self.private_key,
                chain_id=self.chain_id,
                creds=self.creds,
                funder=self.funder_address,
                signature_type=1  # Proxy wallet - Gasless trading
            )
            
            # Wallet cím kiíratása (debug)
            logger.info(f"✅ Polymarket Proxy (Gasless) kapcsolat OK")
            logger.info(f"📍 Működési cím: {self.funder_address}")
            return True
            
        except Exception as e:
            logger.info(f"❌ Polymarket inicializálási hiba: {e}")
            self.client = None
            return False
    
    def get_yes_price(self, token_id: str) -> float | None:
        """
        Lekérdezi egy IGEN token jelenlegi árát.
        Returns: float (pl. 0.72) vagy None ha hiba van
        """
        if not self.client or not token_id:
            return None
        
        try:
            # Orderbook lekérdezése
            order_book = self.client.get_order_book(token_id)  # type: ignore
            bids = order_book.bids if hasattr(order_book, 'bids') else []
            if bids:
                # Legjobb vételi ár
                best_bid = float(bids[0].price if hasattr(bids[0], 'price') else bids[0].get("price", 0))
                return best_bid
            return None
        except Exception as e:
            logger.info(f"❌ Ár lekérdezési hiba: {e}")
            return None

    def get_ask_price(self, token_id: str) -> float | None:
        """
        Lekérdezi a legjobb ELADÁSI árat (amennyiért venni tudunk).
        Fast path: Uses persistent session.
        Returns: float (pl. 0.72) vagy None ha hiba van
        """
        if not token_id:
            return None
        
        try:
            # Orderbook lekérdezése - RAW request for speed
            url = f"{self.host}/book"
            resp = self.session.get(url, params={"token_id": token_id}, timeout=2)
            
            if resp.status_code == 200:
                book = resp.json()
                if book.get("asks"):
                    return float(book["asks"][0]["price"])
            return None
        except Exception:
            return None
    
    def place_buy_order(self, token_id: str, price: float, size: float, dry_run: bool = True, expiration: int = 0) -> dict | None:
        """
        Vételi megbízást ad le (Sync) GTD expiráció támogatással.
        
        Args:
            token_id: A token amit venni akarunk
            price: Maximum ár (pl. 0.75)
            size: Mennyit költünk USDC-ben (pl. 10)
            dry_run: Ha True, csak szimulálja, nem köt valódi ügyletet
            expiration: Lejárat másodpercben (ha 0, nincs GTD Timeout)
        
        Returns: Order dict vagy None
        """
        if not self.client or not token_id:
            logger.info("❌ Nincs Polymarket kapcsolat")
            return None
        
        if dry_run:
            logger.info(f"🧪 [DRY RUN] Vennék IGEN-t:")
            logger.info(f"   Token: {token_id[:20]}...")  # type: ignore
            logger.info(f"   Ár: ${price}")
            logger.info(f"   Összeg: ${size} USDC")
            return {"status": "simulated", "price": price, "size": size}
        
        try:
            # Valódi order leadása GTD időbélyeggel
            import time
            expiration_timestamp = str(int(time.time()) + expiration) if expiration > 0 else "0"
            
            logger.info(f"DEBUG - Token: {token_id} | TTL: {expiration}s")
            logger.info(f"DEBUG - Price: {price}")
            logger.info(f"DEBUG - Size (Shares): {size}")
            
            order_args = OrderArgs(
                token_id=token_id,
                price=price,
                size=size,
                side=BUY,
                expiration=expiration_timestamp
            )
            
            response = self.client.create_and_post_order(order_args)  # type: ignore
            logger.info(f"DEBUG - API Response: {response}")
            
            # Hibakezelés: A válasz lehet lista vagy dict
            resp_data = response[0] if isinstance(response, list) and response else response
            
            if isinstance(resp_data, dict) and ('error_message' in resp_data or 'error' in resp_data):
                 logger.info(f"❌ API Hiba: {resp_data.get('error_message') or resp_data.get('error')}")
                 return None
            
            if isinstance(resp_data, dict) and resp_data.get('success') is False:
                 logger.info(f"❌ Sikertelen order: {resp_data}")
                 return None
                 
            logger.info(f"✅ Order leadva! ID: {resp_data.get('orderID', 'N/A')}")
            return resp_data
            
        except Exception as e:
            logger.info(f"❌ Order kivétel: {e}")
            return None
    
    def place_batch_orders(self, orders: list[dict], dry_run: bool = True) -> list[dict] | None:
        """
        Batch megrendelések küldése a hálózati optimalizálás érdekében Good-Til-Date (expiration) támogatással.
        
        Args:
            orders: Lista [{'token_id': ..., 'price': ..., 'size': ..., 'side': BUY/SELL, 'expiration': 12}, ...]
            dry_run: Ha True, csak szimuláció.
            
        Returns:
            Lista a válasz order dict-ekkel vagy None hiba esetén.
        """
        if not self.client or not orders:
            logger.info("❌ Nincs Polymarket kapcsolat vagy üres as order lista")
            return None
            
        if dry_run:
            logger.info(f"🧪 [DRY RUN] BATCH ({len(orders)} db order) küldése:")
            for o in orders:
                logger.info(f"   - {o['side']} Token: {o['token_id'][:15]}... | Ár: ${o['price']} | Összeg: ${o['size']} USDC")  # type: ignore
            return [{"status": "simulated", **o} for o in orders]
            
        try:
            import time
            signed_orders = []
            now_ts = int(time.time())
            
            for o in orders:
                exp_sec = o.get("expiration", 0)
                expiration_timestamp = str(now_ts + exp_sec) if exp_sec > 0 else "0"
                
                order_args = OrderArgs(
                    token_id=o["token_id"],
                    price=o["price"],
                    size=o["size"],
                    side=o["side"],
                    expiration=expiration_timestamp
                )
                signed_order = self.client.create_order(order_args)  # type: ignore
                signed_orders.append(signed_order)
            
            # Batch API call a CLOB klienssel
            response = []
            if signed_orders:
                response = self.client.post_orders(signed_orders)  # type: ignore
            
            # Hibakezelés batch listára
            if isinstance(response, list):
                successes = []
                for resp in response:
                    if isinstance(resp, dict) and 'orderID' in resp:
                        successes.append(resp)
                    else:
                        logger.info(f"⚠️ Részleges Batch hiba: {resp}")
                
                if successes:
                    logger.info(f"✅ BATCH leadva! ({len(successes)}/{len(orders)} sikeres)")
                    return successes
                return None
            else:
                 logger.info(f"❌ API Batch Hiba formátum: {response}")
                 return None
                 
        except Exception as e:
            logger.info(f"❌ Batch feladási kivétel: {e}")
            return None

    def cancel_batch_orders(self, order_ids: list[str], dry_run: bool = True) -> bool:
        """
        Batch order törlés.
        """
        if not self.client or not order_ids:
            return False
            
        if dry_run:
            logger.info(f"🧪 [DRY RUN] Törlés {len(order_ids)} beragadt limit ordert...")
            return True
            
        try:
            response = self.client.cancel_orders(order_ids)  # type: ignore
            logger.info(f"✅ Törlés sikeres ({len(order_ids)} order).")
            return True
        except Exception as e:
            logger.info(f"❌ Batch törlési kivétel: {e}")
            return False

    def is_connected(self) -> bool:
        """Ellenőrzi, hogy él-e a Polymarket kapcsolat"""
        return self.client is not None


# Teszt
async def test_connection():
    logger.info("🔍 Polymarket kapcsolat tesztelése...")
    client = PolymarketClient()
    
    if client.private_key and client.funder_address:
        success = client.initialize()
        if success:
            logger.info("✅ Polymarket kliens működik (Gasless mode)!")
        else:
            logger.info("❌ Polymarket inicializálás sikertelen")
    else:
        logger.info("❌ Nincs PRIVATE_KEY vagy FUNDER_ADDRESS beállítva a .env-ben!")


if __name__ == "__main__":
    import asyncio
    asyncio.run(test_connection())
