# Polymarket Taker Fee & Maker Rebate (Jan 2026)

## 🚨 Critical Update: Taker Fees Introduced

As of **January 7-8, 2026**, Polymarket introduced **taker fees** specifically for 15-minute crypto UP/DOWN markets.

### Fee Structure (Price-Dependent)

Taker fees are highest around **0.50 price** (~3% effective) and lower at extremes (0.01 or 0.99).

**Example (100 shares @ 0.50):**
```
BUY (taker):  Fee ≈ $0.78  (~1.6% effective)
SELL (taker): Fee ≈ $1.56  (~3.1% effective)
```

**Worst case (~0.30 price):**
```
SELL (taker): Fee ≈ $3.70  (~3.7% effective)
```

Source: [Polymarket Docs - Taker Fees](https://docs.polymarket.com/#taker-fees)

---

## 💰 Maker Rebate Program (WHY MAKER IS KING)

Polymarket introduced a **Maker Rebate Program** to incentivize liquidity provision.

### Rebate Schedule

- **Until Jan 9, 2026**: **100% rebate** of taker fees to makers
- **After Jan 9, 2026**: **XX% rebate** (percentage determined by Polymarket, likely 20-50%)

**How it works:**
1. Takers pay fees (e.g., $1.56 per trade)
2. Those fees are **distributed to makers** who provided liquidity
3. Makers earn **additional profit** beyond the spread

Source: [Polymarket Docs - Maker Rebates](https://docs.polymarket.com/#maker-rebates)

---

## 📊 Strategy Profitability Impact

### Maker-Maker (Our Strategy) ✅ **BEST**

**Entry:** Post-only ladder (mid-1, mid-2, mid-3) → **NO FEE** (we are maker)  
**Exit:** Post-only TP order → **NO FEE** (we are maker)  
**Rebate:** **YES** (we earn from taker fees)

**Break-even:** Only need to cover spread (~0-1 tick in good markets)  
**Profit target:** 2-4 ticks is **highly profitable** after rebates

**Example:**
```
Entry: $0.47 (maker, filled by panic seller)
Exit:  $0.49 (maker, filled by panic buyer)
Gross: $0.02 per share × 20 shares = $0.40
Maker rebate: ≈ $0.10-0.20 (estimate)
Net: $0.50-0.60 profit per trade
```

### Taker-Maker (Hybrid) ⚠️ ACCEPTABLE

**Entry:** Limit order with slippage (taker) → **PAY FEE** (~$0.78 @ 0.50)  
**Exit:** Post-only TP → **NO FEE** (maker) + **EARN REBATE**

**Break-even:** ~0.8 cent movement needed  
**Profit target:** 2-4 ticks still viable, but lower margin

### Taker-Taker (Market Orders) ❌ **DEAD**

**Entry:** Market buy → **PAY FEE** (~$0.78)  
**Exit:** Market sell → **PAY FEE** (~$1.56)  
**Total fees:** ~$2.34 per round trip

**Break-even:** ~2.5-3 cent movement needed  
**Profit target:** 2-4 tick scalps are **unprofitable** after fees

---

## 🔧 Implementation Requirements

### 1. Fee-Rate API Integration (MANDATORY)

All signed orders **must include `fee_rate_bps`** starting Jan 7-8, 2026.

**Failure to include fee rate → Order rejection**

#### Using Official py-clob-client (Recommended)

```bash
# Update to latest version (handles fee automatically)
pip install --upgrade py-clob-client>=0.20.0
```

The official client **automatically fetches and applies** the correct fee rate.

#### Custom Implementation

If using custom order signing:

```python
# Step 1: Fetch fee rate for token
fee_rate_bps = clob_client.get_fee_rate(token_id)

# Step 2: Include in signed order
order = {
    "token_id": token_id,
    "price": "0.47",
    "size": "20",
    "side": "BUY",
    "fee_rate_bps": fee_rate_bps,  # ← CRITICAL
    # ... other params
}

signed_order = sign_order(order, private_key)
```

Source: [CLOB API - Fee Rates](https://docs.polymarket.com/#clob-api-fee-rates)

---

### 2. Net P&L Calculation

**For Maker-Maker strategy (our bot):**

```python
def calculate_net_pnl_maker(entry_price, exit_price, size, maker_rebate_pct=0.5):
    """
    Calculate net P&L for maker-only trades.
    
    Args:
        entry_price: Fill price on entry (maker)
        exit_price: Fill price on exit (maker)
        size: Shares traded
        maker_rebate_pct: Rebate % (0.5 = 50% of taker fees)
    
    Returns:
        Net profit in USD
    """
    gross_profit = (exit_price - entry_price) * size
    
    # Estimate maker rebate (very rough)
    # Assume takers paid ~$0.01-0.02 per share in fees
    estimated_rebate = size * 0.015 * maker_rebate_pct
    
    net_profit = gross_profit + estimated_rebate
    return net_profit

# Example:
entry = 0.47
exit = 0.49
size = 20

net = calculate_net_pnl_maker(entry, exit, size)
# Gross: $0.40
# Rebate: ~$0.15
# Net: ~$0.55
```

**Important:** Actual rebate calculation is complex (depends on global taker volume). Track real rebates via Polymarket API.

---

### 3. Fee-Aware Order Validation

Before placing orders, validate that:

1. **Post-only flag is set** (to ensure maker status)
2. **Price does not cross spread** (would trigger taker)
3. **Fee rate is included** (API requirement)

```python
def validate_maker_order(price, side, current_book):
    """Ensure order will be maker, not taker"""
    if side == "BUY":
        if price >= current_book.best_ask:
            raise ValueError("Order would cross spread (taker)")
    elif side == "SELL":
        if price <= current_book.best_bid:
            raise ValueError("Order would cross spread (taker)")
    
    return True
```

---

## 🎯 Why This Makes Maker Strategy Even Better

| Factor | Impact |
|--------|--------|
| **No taker fees** | Save ~1.6-3.7% per trade |
| **Maker rebates** | Earn ~0.5-1% extra per trade |
| **Better queue position** | Fill at better prices (mid-based ladder) |
| **Lower break-even** | Only need to beat spread, not fees |
| **Sustainable edge** | Taker-based bots now unprofitable → less competition |

---

## 📝 Action Items for Bot

- [x] Document fee structure and maker rebate program
- [ ] Ensure py-clob-client >= 0.20.0 in requirements.txt
- [ ] Add fee-rate fetching to order placement logic
- [ ] Update P&L calculation to include maker rebates
- [ ] Add validation to prevent accidental taker orders
- [ ] Monitor actual rebate earnings via Polymarket API

---

## 🔗 References

- [Polymarket Taker Fees Announcement](https://docs.polymarket.com)
- [Maker Rebate Program Details](https://docs.polymarket.com/#maker-rebates)
- [CLOB API Fee Rate Documentation](https://docs.polymarket.com/#clob-api-fee-rates)
- [Fee Calculation Examples](https://docs.polymarket.com/#fee-examples)

---

**Bottom Line:** The introduction of taker fees makes our **maker-only ambush ladder strategy** significantly more profitable than taker-based approaches. We should aggressively pursue maker fills and avoid taker orders at all costs.
