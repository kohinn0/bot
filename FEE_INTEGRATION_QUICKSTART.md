# Fee-Rate Integration Quick Start

## ⚡ TL;DR - Critical Steps

1. **Fetch fee rate** for token BEFORE signing
2. **Add `feeRateBps` to order dict** as string
3. **Sign order** (with fee included)
4. **Submit** to CLOB

**WRONG:**
```python
order = sign_order({"price": "0.47", ...})  # ❌ No fee
order["feeRateBps"] = "1000"  # ❌ Too late!
post_order(order)  # ❌ Signature mismatch
```

**RIGHT:**
```python
fee = fetch_fee_rate_bps(token_id)  # ✅ Step 1
order = {"price": "0.47", "feeRateBps": str(fee), ...}  # ✅ Step 2
signed = sign_order(order)  # ✅ Step 3 (fee already in)
post_order(signed)  # ✅ Step 4
```

---

## 🚀 Using Official Client (Easiest)

```bash
pip install --upgrade py-clob-client>=0.21.0
```

```python
from py_clob_client.client import ClobClient

client = ClobClient(host="...", key=private_key, chain_id=137)

# Fee is handled AUTOMATICALLY
order = client.create_order(OrderArgs(
    price=0.47,
    size=20,
    side="BUY",
    token_id=token_id,
    post_only=True  # Maker order
))

response = client.post_order(order)
```

---

## 🔧 Custom Implementation

```python
from fee_utils import fetch_fee_rate_bps, add_fee_to_order

# Method 1: Fetch and add manually
token_id = "123456..."
fee_bps = fetch_fee_rate_bps(token_id)  # Returns int (e.g., 1000)

order = {
    "tokenId": token_id,
    "price": "0.47",
    "size": "20",
    "feeRateBps": str(fee_bps),  # ← Must be string!
    # ... other fields
}

signed_order = your_sign_function(order)
post_to_clob(signed_order)

# Method 2: Helper function
order = {"tokenId": token_id, "price": "0.47", ...}
order = add_fee_to_order(order, token_id)  # Auto-fetches and adds
signed_order = your_sign_function(order)
```

---

## 📋 Complete Example: Ambush Ladder

```python
from fee_utils import fetch_fee_rate_bps
from py_clob_client.client import ClobClient
from py_clob_client.clob_types import OrderArgs

# Setup
client = ClobClient(...)
token_id = "..."
mid_price = 0.48

# Fetch fee once for all levels
fee_bps = fetch_fee_rate_bps(token_id)

# Place 3-level ladder
levels = [
    {"price": 0.47, "size": 10},  # mid - 1 tick
    {"price": 0.46, "size": 6},   # mid - 2 ticks
    {"price": 0.45, "size": 4},   # mid - 3 ticks
]

for level in levels:
    order = client.create_order(OrderArgs(
        price=level["price"],
        size=level["size"],
        side="BUY",
        token_id=token_id,
        fee_rate_bps=str(fee_bps),  # Same fee for all
        post_only=True
    ))
    
    response = client.post_order(order)
    print(f"✅ Level placed: {level['price']} → {response['orderID']}")
```

---

## 🛠️ Debugging Tips

### "Signature Invalid" Error
→ Fee was added AFTER signing, not BEFORE  
→ Fix: Add `feeRateBps` to order dict before calling sign function

### "Missing feeRateBps" Error
→ Fee field not included in order  
→ Fix: Call `fetch_fee_rate_bps()` and add to order

### Type Error (int vs string)
→ `feeRateBps` must be STRING, not int  
→ Fix: `order["feeRateBps"] = str(fee_bps)`

---

## 📊 Fee Impact (Reminder)

| Order Type | Fee Cost | Our Strategy |
|------------|----------|--------------|
| Maker (post-only) | **$0** + rebate | ✅ **Always use** |
| Taker (market) | **~$0.78-1.56** @ 0.50 | ❌ Never use |

---

## 📁 Files

- `fee_utils.py` - Utility functions
- `order_signing_example.py` - Complete examples
- `TAKER_FEE_IMPACT.md` - Detailed fee analysis

---

## ✅ Checklist

- [x] Fetch fee rate from `/fee-rate?token_id=...`
- [x] Add `feeRateBps` as **string** to order dict
- [x] Sign order (with fee already included)
- [x] Use **post-only** flag for maker orders
- [x] Update py-clob-client to >= 0.21.0
