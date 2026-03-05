"""
Polymarket CLOB Fee-Rate Utility
Fetches fee rates for tokens and integrates into order signing.
"""
import requests
from typing import Dict, Any
import logging

logger = logging.getLogger(__name__)

# Polymarket CLOB endpoints
CLOB_BASE_URL = "https://clob.polymarket.com"
FEE_RATE_ENDPOINT = f"{CLOB_BASE_URL}/fee-rate"


def fetch_fee_rate_bps(token_id: str, timeout: int = 2) -> int:
    """
    Fetch fee rate in basis points for a given token.
    
    Args:
        token_id: The Polymarket token ID (e.g., "12345...")
        timeout: Request timeout in seconds
    
    Returns:
        Fee rate in basis points (e.g., 1000 = 10%)
        Returns 0 for fee-free markets
    
    Raises:
        requests.HTTPError: If API request fails
        KeyError: If response doesn't contain fee_rate_bps
    
    Example:
        >>> fee = fetch_fee_rate_bps("123456789")
        >>> print(f"Fee: {fee / 100}%")  # Convert bps to %
        Fee: 10.0%
    """
    try:
        response = requests.get(
            FEE_RATE_ENDPOINT,
            params={"token_id": token_id},
            timeout=timeout
        )
        response.raise_for_status()
        
        data = response.json()
        fee_bps = int(data["fee_rate_bps"])
        
        logger.info(f"Fetched fee rate for token {token_id}: {fee_bps} bps")
        return fee_bps
    
    except requests.RequestException as e:
        logger.error(f"Failed to fetch fee rate for token {token_id}: {e}")
        raise
    except (KeyError, ValueError) as e:
        logger.error(f"Invalid fee rate response for token {token_id}: {e}")
        raise


def add_fee_to_order(order: Dict[str, Any], token_id: str) -> Dict[str, Any]:
    """
    Fetch fee rate and add to order dict BEFORE signing.
    
    CRITICAL: The feeRateBps field must be included BEFORE signing the order.
    If added after signing, the signature validation will fail.
    
    Args:
        order: Order dict with tokenId, price, size, etc.
        token_id: Token ID to fetch fee for
    
    Returns:
        Order dict with feeRateBps added (as string)
    
    Example:
        >>> order = {
        ...     "tokenId": "123456",
        ...     "price": "0.47",
        ...     "size": "20",
        ...     "side": "BUY"
        ... }
        >>> order_with_fee = add_fee_to_order(order, "123456")
        >>> print(order_with_fee["feeRateBps"])
        "1000"
    """
    fee_bps = fetch_fee_rate_bps(token_id)
    
    # IMPORTANT: Must be string, not int
    order["feeRateBps"] = str(fee_bps)
    
    logger.debug(f"Added feeRateBps={fee_bps} to order for token {token_id}")
    return order


def estimate_taker_fee_usd(
    price: float,
    size: int,
    fee_rate_bps: int
) -> float:
    """
    Estimate taker fee in USD.
    
    Note: This is approximate. Actual fee calculation is complex
    and depends on the specific market type.
    
    Args:
        price: Order price (0.0 - 1.0)
        size: Number of shares
        fee_rate_bps: Fee rate in basis points
    
    Returns:
        Estimated fee in USD
    
    Example:
        >>> # 100 shares @ 0.50 with 1000 bps fee
        >>> fee = estimate_taker_fee_usd(0.50, 100, 1000)
        >>> print(f"${fee:.2f}")
        $1.56
    """
    # Simplified calculation - actual Polymarket fee is more complex
    # For price-dependent fees, see TAKER_FEE_IMPACT.md
    notional = price * size
    fee_usd = notional * (fee_rate_bps / 10000)
    
    return fee_usd


if __name__ == "__main__":
    # Test fee rate fetching
    print("Testing Fee Rate Utility\n")
    print("=" * 60)
    
    # Example token ID (replace with real one)
    test_token_id = "71321045679252212594626385532706912750332728571942532289631379312455583992833"
    
    try:
        fee_bps = fetch_fee_rate_bps(test_token_id)
        print(f"✅ Fee rate fetched: {fee_bps} bps ({fee_bps/100}%)")
        
        # Estimate fee for typical trade
        fee_usd = estimate_taker_fee_usd(0.50, 100, fee_bps)
        print(f"📊 Estimated fee for 100 shares @ 0.50: ${fee_usd:.2f}")
        
        # Test order integration
        test_order = {
            "tokenId": test_token_id,
            "price": "0.47",
            "size": "20",
            "side": "BUY"
        }
        
        order_with_fee = add_fee_to_order(test_order, test_token_id)
        print(f"\n✅ Order with fee:")
        print(f"   feeRateBps: {order_with_fee['feeRateBps']}")
        
    except Exception as e:
        print(f"❌ Error: {e}")
        print("\nNote: This test requires a valid token_id from an active Polymarket market.")
