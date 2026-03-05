from bot_logger import logger
import json
import re
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any, Dict, List, Optional, Tuple

import requests

# Compatibility with new config
GAMMA_BASE_URL = "https://gamma-api.polymarket.com"
MARKET_SEARCH_QUERY = "bitcoin"
BTC_15M_SLUG_REGEX = r"(btc-updown-15m-\d+|bitcoin-up-or-down-.*)"
MARKET_LOOKUP_TTL_SEC = 2.0

from binance_feed import BinanceFeed


def _parse_iso_utc(ts: str) -> datetime:
    ts = ts.strip()
    if ts.endswith("Z"):
        ts = ts[:-1] + "+00:00"
    dt = datetime.fromisoformat(ts)
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    return dt.astimezone(timezone.utc)


@dataclass(frozen=True)
class MarketContext:
    market_id: str
    slug: str
    asset: str           # NEW: 'BTC', 'ETH', 'SOL', 'XRP'
    start: datetime
    end: datetime
    s0_truth: float
    up_token_id: str
    down_token_id: str
    tick_size: float     # NEW: Dynamic decimal precision


class MarketFinder:
    def __init__(self, session: Optional[requests.Session] = None):
        self.session = session or requests.Session()
        self._slug_re = re.compile(BTC_15M_SLUG_REGEX)
        self._cache: Optional[Tuple[float, MarketContext]] = None
        self._s0_cache: Dict[str, float] = {}

    def get_active_market(self, binance: BinanceFeed) -> Optional[MarketContext]:
        now = time.time()
        if self._cache and (now - self._cache[0] < MARKET_LOOKUP_TTL_SEC):
            return self._cache[1]

        # 1. Discover the best active market (Lite object from Search)
        market_lite = self._discover_latest_active_slug()
        if not market_lite:
            return None
            
        slug = market_lite.get("slug")
        if not slug:
            return None

        # 2. Fetch the FULL market details (with IDs) using the slug
        # The search result 'market_lite' is missing 'id' and 'clobTokenIds'!
        market_full = self._fetch_market_by_slug(slug)
        if not market_full:
            logger.info(f"⚠️ Could not fetch full details for slug: {slug}")
            return None

        # 3. Convert full market to context
        ctx = self._to_context(market_full, binance)
        if not ctx:
            return None

        self._cache = (now, ctx)
        return ctx

    def _discover_latest_active_slug(self) -> Optional[Dict[str, Any]]:
        try:
            r = self.session.get(
                f"{GAMMA_BASE_URL}/public-search",
                params={
                    "q": MARKET_SEARCH_QUERY, # Updated: Use config (was Hardcoded "15m")
                    "limit_per_type": 50,
                    "page": 1,
                    "keep_closed_markets": 0,
                    "optimized": "true",
                },
                timeout=5,
            )
            r.raise_for_status()
            payload = r.json()
            
            # Gamma API returns 'events' list
            events = payload.get("events") or []
            if not events and isinstance(payload, list):
                events = payload
                
            candidates = []
            
            for e in events:
                slug = (e.get("slug") or "").strip()
                if not slug: continue
                
                # Debug logging
                # logger.info(f"DEBUG Check Slug: {slug}")
                
                # Filter for "Up or Down" markets
                # Allow 'bitcoin-up-or-down', 'btc-updown', 'btc-up-or-down'
                if not any(x in slug for x in ["bitcoin", "btc"]):
                    continue
                    
                # Enforce Regex Match
                if not self._slug_re.match(slug):
                    # Debug: print why rejected?
                    if "btc" in slug and "15m" not in slug:
                        # logger.info(f"DEBUG: Rejected by Regex: {slug}")
                        pass
                    continue
                    
                # Skip if closed/inactive
                if e.get("closed") is True: continue
                if e.get("active") is False: continue
                
                # Check End Date
                end_iso = e.get("endDate") or e.get("end_date")
                if not end_iso: continue
                
                try:
                    # quick parse to verify it's valid
                    # (logic in _to_context does full parse)
                    candidates.append((slug, end_iso))
                except:
                    continue

            if not candidates:
                logger.info("DEBUG: No candidates after filtering.")
                return None

            # logger.info(f"DEBUG: Found {len(candidates)} valid candidates.")
            
            # Sort by End Date (find the one ending soonest but in future? or just started?)
            # Since Gamma returns active markets, we just want the nearest one.
            # But wait, date string sort is dangerous if format differs. 
            # We will interpret date in the loop or assume API sort?
            # Gamma usually returns sorted or we can trust the list order? 
            # Let's rely on the first one or better yet, verify via timestamp
            
            # Let's actually parse them to sort correctly
            def parse_ts(c):
                try:
                    dt = _parse_iso_utc(c[1])
                    return dt.timestamp()
                except:
                    return 9999999999
            
            # Filter out expired ones
            now_ts = time.time()
            valid_candidates = []
            
            for slug, end_s in candidates:
                ts = parse_ts((slug, end_s))
                if ts > now_ts:
                    valid_candidates.append({'slug': slug, 'ts': ts, 'is_15m': '15m' in slug})
            
            if not valid_candidates:
                logger.info("DEBUG: No valid candidates found (all expired?).")
                return None
            
            # Prioritize 15m markets!
            # If we have ANY 15m market, filter out non-15m markets
            has_15m = any(c['is_15m'] for c in valid_candidates)
            if has_15m:
                # logger.info("DEBUG: Found 15m markets! Filtering for 15m only.")
                valid_candidates = [c for c in valid_candidates if c['is_15m']]
            
            # Sort by time asc (nearest deadlines first)
            valid_candidates.sort(key=lambda x: x['ts'])
            
            best_slug = valid_candidates[0]['slug']
            logger.info(f"DEBUG: Best Candidate: {best_slug} (Ends: {datetime.fromtimestamp(valid_candidates[0]['ts']).strftime('%H:%M:%S')})")
            
            # Find the event object again
            for e in events:
                if e.get("slug") == best_slug:
                     return e
            return None
            
        except Exception as e:
            logger.info(f"⚠️ Market Discovery Error: {e}")
            return None

    def _fetch_market_by_slug(self, slug: str) -> Optional[Dict[str, Any]]:
        try:
            r = self.session.get(
                f"{GAMMA_BASE_URL}/markets",
                params={"slug": slug},
                timeout=5,
            )
            r.raise_for_status()
            data = r.json()
            if isinstance(data, list) and data:
                return data[0]
            if isinstance(data, dict) and data.get("markets"):
                mkts = data["markets"]
                if isinstance(mkts, list) and mkts:
                    return mkts[0]
            return None
        except Exception as e:
            logger.info(f"⚠️ Fetch Slug Error: {e}")
            return None

    def _to_context(self, market: Dict[str, Any], binance: BinanceFeed) -> Optional[MarketContext]:
        try:
            market_id = str(market.get("id") or "")
            slug = str(market.get("slug") or "")
            if not market_id or not slug:
                with open("debug_error.txt", "a") as f: f.write(f"Missing ID or Slug. ID:{market_id} Slug:{slug}\n")
                return None

            start_s = market.get("startDate") or market.get("start_date")
            end_s = market.get("endDate") or market.get("end_date")
            if not start_s or not end_s:
                with open("debug_error.txt", "a") as f: f.write("Missing Start/End Date\n")
                return None

            start = _parse_iso_utc(str(start_s))
            end = _parse_iso_utc(str(end_s))

            token_ids = market.get("clobTokenIds") or market.get("clob_token_ids")
            if isinstance(token_ids, str):
                try:
                    token_ids = json.loads(token_ids)
                except:
                    pass
            
            outcomes = market.get("outcomes")
            if isinstance(outcomes, str):
                 try:
                     outcomes = json.loads(outcomes)
                 except:
                     pass
            if not isinstance(token_ids, list) or len(token_ids) < 2:
                with open("debug_error.txt", "a") as f: f.write(f"Invalid Token IDs: {token_ids}\n")
                return None
            if not isinstance(outcomes, list) or len(outcomes) < 2:
                with open("debug_error.txt", "a") as f: f.write(f"Invalid Outcomes: {outcomes}\n")
                return None

            up_token_id, down_token_id = self._map_up_down_tokens(outcomes, token_ids)
            if not up_token_id or not down_token_id:
                with open("debug_error.txt", "a") as f: f.write(f"outcome mapping failed. Outs:{outcomes} Tids:{token_ids}\n")
                return None

            s0 = self._resolve_s0_truth(market, start, binance)
            if s0 is None or s0 <= 0:
                with open("debug_error.txt", "a") as f: f.write(f"S0 Resolution Failed for {slug}\n")
                return None

            # Detect asset type from slug
            asset = 'BTC'  # default
            slug_lower = slug.lower()
            if 'ethereum' in slug_lower or 'eth' in slug_lower:
                asset = 'ETH'
            elif 'solana' in slug_lower or 'sol' in slug_lower:
                asset = 'SOL'
            elif 'xrp' in slug_lower:
                asset = 'XRP'
            elif 'bitcoin' in slug_lower or 'btc' in slug_lower:
                asset = 'BTC'

            # Extract tick size from market metadata
            # Default to 0.01 if not specified (most common format)
            tick_size_str = market.get("minimumOrderSize") or market.get("minimumTickSize") or market.get("tickSize") or "0.01"
            try:
                tick_size = float(tick_size_str)
            except ValueError:
                tick_size = 0.01

            return MarketContext(
                market_id=market_id,
                slug=slug,
                asset=asset,  # NEW: Asset detection
                start=start,
                end=end,
                s0_truth=float(s0),
                up_token_id=str(up_token_id),
                down_token_id=str(down_token_id),
                tick_size=tick_size  # NEW: Dynamic precision
            )
        except Exception as e:
            logger.info(f"⚠️ Context Conversion Error: {e}")
            import traceback
            traceback.print_exc()
            return None

    @staticmethod
    def _map_up_down_tokens(outcomes: List[Any], token_ids: List[Any]) -> Tuple[Optional[str], Optional[str]]:
        outs = [str(o).strip().lower() for o in outcomes[:2]]
        tids = [str(t).strip() for t in token_ids[:2]]

        def is_up(x: str) -> bool:
            return x in {"up", "higher", "increase", "bull", "above"}

        def is_down(x: str) -> bool:
            return x in {"down", "lower", "decrease", "bear", "below"}

        if is_up(outs[0]) and is_down(outs[1]):
            return tids[0], tids[1]
        if is_down(outs[0]) and is_up(outs[1]):
            return tids[1], tids[0]

        return tids[0], tids[1]

    def _resolve_s0_truth(self, market: Dict[str, Any], start: datetime, binance: BinanceFeed) -> Optional[float]:
        slug = str(market.get("slug") or "")
        
        # 1. Try In-Memory Cache first
        if slug in self._s0_cache:
            return self._s0_cache[slug]

        # 2. Try API Metadata 
        val = None
        for k in ("startPrice", "start_price", "referencePrice", "reference_price"):
            if k in market and market[k] not in (None, ""):
                try:
                    v = float(market[k])
                    if v > 0:
                        val = v
                        break
                except Exception:
                    pass
        
        # 3. Fallback to Binance History at Timestamp
        if val is None or val <= 0:
            ts_ms = int(start.timestamp() * 1000)
            logger.info(f"🔍 Fetching S0 from Binance History: {ts_ms}")
            try:
                val = binance.get_price_at_ms(ts_ms)
            except Exception as e:
                logger.info(f"⚠️ Binance History Fetch Failed: {e}")
                # Fallback to last known good value if implies stability vs crash
                pass

        if val and val > 0:
            self._s0_cache[slug] = val
            return val
            
        return None

if __name__ == "__main__":
    from binance_feed import BinanceFeed
    
    logger.info("🔬 MARKET FINDER DIAGNOSTICS")
    binance = BinanceFeed()
    finder = MarketFinder()
    
    logger.info("1. Searching for active market...")
    market_lite = finder._discover_latest_active_slug()
    
    if market_lite:
        slug = market_lite.get("slug")
        logger.info(f"✅ FOUND SLUG: {slug}")
        
        logger.info("2. Fetching FULL market details...")
        market_full = finder._fetch_market_by_slug(slug)
        
        if market_full:
            logger.info(f"✅ FULL MARKET FETCHED. ID: {market_full.get('id')}")
            
            logger.info("3. Converting to Context...")
            ctx = finder._to_context(market_full, binance)
            if ctx:
                logger.info(f"✅ CONTEXT SUCCESS: {ctx}")
            else:
                logger.info("❌ CONTEXT FAILED")
        else:
            logger.info("❌ FULL FETCH FAILED")
    else:
        logger.info("❌ NO MARKET FOUND")
