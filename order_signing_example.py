"""
Example: Proper Order Signing with Fee Rate
Demonstrates the correct order of operations for Polymarket orders.
"""
from py_clob_client.client import ClobClient
from py_clob_client.clob_types import OrderArgs
from fee_utils import fetch_fee_rate_bps
import os
from dotenv import load_dotenv

load_dotenv()

# Initialize CLOB client
client = ClobClient(
    host="https://clob.polymarket.com",
    key=os.getenv("PRIVATE_KEY"),
    chain_id=137  # Polygon
)


def create_maker_order_with_fee(
    token_id: str,
    price: str,
    size: str,
    side: str  # "BUY" or "SELL"
) -> dict:
    """
    Create a maker (post-only) order with proper fee rate integration.
    
    CRITICAL ORDER OF OPERATIONS:
    1. Fetch fee rate
    2. Build order dict with feeRateBps
    3. Sign the order (with fee included)
    4. Submit to CLOB
    
    Args:
        token_id: Polymarket token ID
        price: Limit price (e.g., "0.47")
        size: Number of shares (e.g., "20")
        side: "BUY" or "SELL"
    
    Returns:
        API response from order creation
    """
    
    # Step 1: Fetch fee rate for this token
    fee_bps = fetch_fee_rate_bps(token_id)
    print(f"📊 Fee rate: {fee_bps} bps")
    
    # Step 2: Build order arguments
    # Using official py-clob-client (v0.21.0+) automatically handles fee
    order_args = OrderArgs(
        price=float(price),
        size=float(size),
        side=side.upper(),
        token_id=token_id,
        fee_rate_bps=str(fee_bps),  # ← CRITICAL: Include before signing
        # For maker orders:
        post_only=True  # Ensures maker status (no taker fee risk)
    )
    
    # Step 3: Create and sign order
    # The official client handles signing with fee included
    signed_order = client.create_order(order_args)
    
    print(f"✅ Order created and signed (with feeRateBps={fee_bps})")
    
    # Step 4: Submit order
    response = client.post_order(signed_order)
    
    print(f"🚀 Order submitted: {response.get('orderID')}")
    return response


def create_ambush_ladder_with_fees(
    token_id: str,
    mid_price: float,
    total_size: int,
    side: str
) -> list:
    """
    Create 3-level ambush ladder with proper fee handling.
    
    Example:
        mid_price = 0.48
        Creates orders at: 0.47, 0.46, 0.45 (for BUY side)
    
    Args:
        token_id: Polymarket token ID
        mid_price: Current mid price
        total_size: Total shares to distribute across levels
        side: "BUY" or "SELL"
    
    Returns:
        List of order responses
    """
    # Fetch fee once (same for all levels)
    fee_bps = fetch_fee_rate_bps(token_id)
    print(f"📊 Fee rate: {fee_bps} bps\n")
    
    # Ladder configuration (from config)
    levels = [
        {"offset_ticks": -1, "size_pct": 0.5},  # mid - 1 tick
        {"offset_ticks": -2, "size_pct": 0.3},  # mid - 2 ticks
        {"offset_ticks": -3, "size_pct": 0.2},  # mid - 3 ticks
    ]
    
    tick_size = 0.01
    orders = []
    
    for i, level in enumerate(levels, 1):
        # Calculate price
        offset = level["offset_ticks"]
        if side == "BUY":
            price = mid_price + (offset * tick_size)
        else:  # SELL
            price = mid_price + (-offset * tick_size)
        
        # Calculate size
        size = int(total_size * level["size_pct"])
        
        print(f"Level {i}: {size} shares @ ${price:.2f}")
        
        # Create order with fee
        order_args = OrderArgs(
            price=price,
            size=size,
            side=side.upper(),
            token_id=token_id,
            fee_rate_bps=str(fee_bps),
            post_only=True
        )
        
        signed_order = client.create_order(order_args)
        response = client.post_order(signed_order)
        
        orders.append(response)
        print(f"  ✅ Order ID: {response.get('orderID')}\n")
    
    return orders


# ============================================================
# MANUAL ORDER SIGNING (if NOT using py-clob-client)
# ============================================================

def manual_order_signing_example(token_id: str):
    """
    Example of manual order signing with fee (for custom implementations).
    
    WARNING: Only use this if you're implementing custom order signing.
    The official py-clob-client is recommended.
    """
    from eth_account import Account
    from eth_account.messages import encode_structured_data
    
    # Step 1: Fetch fee
    fee_bps = fetch_fee_rate_bps(token_id)
    
    # Step 2: Build EIP-712 order payload WITH fee
    order = {
        "maker": "0xYourAddress...",
        "taker": "0x0000000000000000000000000000000000000000",
        "tokenId": token_id,
        "makerAmount": "20000000",  # 20 shares * 1e6
        "takerAmount": "9400000",   # 0.47 price * 20 shares * 1e6
        "side": "BUY",
        "feeRateBps": str(fee_bps),  # ← CRITICAL: Must be string
        "nonce": "123456789",
        "expiration": "1234567890",
        "signatureType": 0
    }
    
    # Step 3: Create EIP-712 structured data
    # (See Polymarket docs for exact domain/types)
    domain = {
        "name": "Polymarket CTF Exchange",
        "version": "1",
        "chainId": 137,
        "verifyingContract": "0x..."
    }
    
    types = {
        "Order": [
            {"name": "maker", "type": "address"},
            {"name": "taker", "type": "address"},
            {"name": "tokenId", "type": "uint256"},
            {"name": "makerAmount", "type": "uint256"},
            {"name": "takerAmount", "type": "uint256"},
            {"name": "side", "type": "uint8"},
            {"name": "feeRateBps", "type": "uint256"},  # ← Fee in typed data
            {"name": "nonce", "type": "uint256"},
            {"name": "expiration", "type": "uint256"},
            {"name": "signatureType", "type": "uint8"}
        ]
    }
    
    structured_data = {
        "domain": domain,
        "types": types,
        "primaryType": "Order",
        "message": order
    }
    
    # Step 4: Sign
    private_key = os.getenv("PRIVATE_KEY")
    account = Account.from_key(private_key)
    
    encoded = encode_structured_data(structured_data)
    signed_message = account.sign_message(encoded)
    
    # Step 5: Build final payload
    signed_order = {
        **order,
        "signature": signed_message.signature.hex()
    }
    
    print("✅ Manually signed order with fee:")
    print(f"   feeRateBps: {signed_order['feeRateBps']}")
    print(f"   signature: {signed_order['signature'][:20]}...")
    
    return signed_order


if __name__ == "__main__":
    print("=" * 60)
    print("POLYMARKET ORDER SIGNING WITH FEE RATE")
    print("=" * 60)
    print()
    
    # Example: Create a single maker order
    print("Example 1: Single Maker Order\n")
    
    # Replace with actual token ID from active market
    test_token_id = "71321045679252212594626385532706912750332728571942532289631379312455583992833"
    
    try:
        # Uncomment to test with real API:
        # response = create_maker_order_with_fee(
        #     token_id=test_token_id,
        #     price="0.47",
        #     size="20",
        #     side="BUY"
        # )
        
        print("\n" + "=" * 60)
        print("Example 2: Ambush Ladder (3 levels)\n")
        
        # Uncomment to test:
        # orders = create_ambush_ladder_with_fees(
        #     token_id=test_token_id,
        #     mid_price=0.48,
        #     total_size=20,
        #     side="BUY"
        # )
        
        print("✅ Examples completed!")
        print("\nNote: Uncomment function calls to test with real API")
        
    except Exception as e:
        print(f"❌ Error: {e}")
