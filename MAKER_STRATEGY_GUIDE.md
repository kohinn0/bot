# Maker Strategy - Professional Guidelines

## Core Philosophy: "Be the Trap, Don't Chase"

Maker profitability comes from being **positioned ahead** of the panic flow, not chasing it. You are the ambush, not the hunter.

---

## 🎯 Ambush Ladder Setup (Simplest Pro Approach)

### Entry Ladder Positioning

**DON'T:** Place at bestBid (too competitive, may get picked off)  
**DO:** Place at mid-price offsets for better adverse selection protection

```
Level 1: mid - 1 tick  (50% of size)
Level 2: mid - 2 ticks (30% of size)
Level 3: mid - 3 ticks (20% of size)
```

**Why mid-based?** 
- Less toxic flow (you're not "obvious")
- Better price improvement vs bestBid
- Natural buffer against adverse selection

### Example

```
Polymarket DOWN token orderbook:
  Best Ask: 0.50
  Best Bid: 0.46
  Mid: 0.48

Your ladder (BUY DOWN):
  Level 1: 0.47 (mid 0.48 - 1 tick)  → 10 shares
  Level 2: 0.46 (mid 0.48 - 2 ticks) → 6 shares
  Level 3: 0.45 (mid 0.48 - 3 ticks) → 4 shares

Total: 20 shares exposure, avg entry if all fill: ~0.465
```

---

## ⚡ Immediate Exit Logic

### On ANY Ladder Level Fill:

1. **Cancel all other entry orders** (don't double inventory)
2. **Place post-only SELL immediately**:
   - Price: `entry_price + max(2 ticks, 0.7 * spread)`
   - Example: Filled at 0.47 → TP at 0.49 (2 ticks profit)
3. **Start time-stop timer**: 6-12 seconds max hold
4. **NO settlement speculation** - harvest repricing wave, not final outcome

### Exit Priorities

```
Priority 1: Take profit fills (best case)
Priority 2: Time-stop (6-12s) → close at market
Priority 3: Reversal stop (Binance retraces 50%+) → close immediately
```

---

## 🛡️ Critical Risk Guards

### 1. Adverse Selection Protection (Toxic Flow Detection)

**Problem:** Makers get filled when informed traders know something you don't.

**Detection:**
```python
if binance_velocity > 0.25%_per_second:  # Strong directional flow
    if impulse_duration > 2_seconds:      # Sustained, not spike
        → WIDEN ladder offsets or SKIP entry
```

**Rule:** If Binance shows 0.2-0.3%/sec sustained momentum → you're likely being picked off. Don't place ladder.

### 2. Spread + Depth Check

**Before placing ladder:**
```python
spread = ask - bid
top_3_levels_liquidity = sum(size for level in [1,2,3])

if spread > 0.04:  # Too wide
    SKIP
if top_3_levels_liquidity < your_size * 3:  # Not enough depth for exit
    SKIP
```

**Why:** Easy fill, impossible exit = trapped.

### 3. Time-to-Resolution Scaling

```python
if time_to_resolution < 90s:
    profit_target *= 0.6    # Lower target
    time_stop = 6000ms      # Faster exit
    position_size *= 0.7    # Smaller size

elif time_to_resolution < 180s:
    profit_target *= 0.8
    time_stop = 9000ms
    position_size *= 0.9

else:  # > 300s
    profit_target *= 1.0
    time_stop = 12000ms
    position_size *= 1.0
```

**Logic:** Less time = higher "stuck" risk → be more aggressive on exit, conservative on entry.

---

## 📊 Order Management Discipline

### Rate Limiting (Critical for Compliance)

```python
MIN_TIME_BETWEEN_ORDER_UPDATES = 250ms  # Absolute minimum
RECOMMENDED = 500ms                      # Safer

# Example:
last_order_timestamp = None

def can_update_orders():
    if last_order_timestamp is None:
        return True
    return (now() - last_order_timestamp) >= 250ms
```

**Rule:** Never refresh orders faster than 250ms. Polymarket will flag you as spam.

### Fill-then-Cancel Pattern

```python
on_ladder_fill(level, filled_size):
    # 1. Cancel competing entries FIRST
    cancel_all_unfilled_ladder_levels()
    
    # 2. THEN place exit
    place_post_only_take_profit(
        price=filled_price + profit_target,
        size=filled_size
    )
    
    # 3. Start timers
    start_time_stop_timer()
```

**Critical:** Cancel → Exit → Timer (in that order). OCO logic prevents double-fill.

---

## 💰 Inventory & Position Controls

### Single Position Rule

```python
MAX_OPEN_POSITIONS = 1  # Never more than 1 at once

if current_position is not None:
    SKIP all new triggers until position closed
```

**Why:** Multi-position = complex hedging + increased risk. Start simple.

### Bankroll Exposure

```python
MAX_POSITION_SIZE = bankroll * 0.05  # Conservative
MAX_POSITION_SIZE = bankroll * 0.10  # Aggressive

# Example: $500 bankroll
conservative_max = $25
aggressive_max = $50
```

### Partial Fill Handling

```python
if filled_size < (total_ladder_size * 0.3):  # Less than 30% filled
    ACTION: Cancel unfilled, close partial at market, log skip
    REASON: Position too small to manage profitably after fees
```

---

## 🔧 Technical Precision (Prevents Hidden Rejects)

### Tick Size Compliance

```python
TICK_SIZE = 0.01  # Polymarket standard

def round_to_tick(price):
    return round(price / TICK_SIZE) * TICK_SIZE

# Example:
raw_price = 0.4734
valid_price = round_to_tick(raw_price)  # → 0.47
```

### Minimum Share Size

```python
MIN_SHARES = 5  # Per Polymarket rules

if calculated_shares < MIN_SHARES:
    SKIP order placement
```

### Post-Only Validation

```python
def place_post_only_order(price, size, side):
    current_book = get_orderbook()
    
    if side == "BUY":
        if price >= current_book.best_ask:
            REJECT: "Would cross spread - not post-only compliant"
            return False
    
    # Place order
    place_order(price, size, side, post_only=True)
```

**Critical:** Post-only orders that would immediately match get rejected. Always validate against current book.

---

## 🧠 The Golden Rule

> **"Maker is good when YOU are the trap."**
> 
> If you feel like you need to "chase the price," it's already a taker game. As a human, you lose that race.

### Decision Tree

```
Binance trigger fires
    ↓
Is impulse speed < 0.25%/sec? ────NO───→ SKIP (toxic flow likely)
    ↓ YES
Is Polymarket spread < 4%? ───────NO───→ SKIP (too wide)
    ↓ YES
Is top-3 depth > 3x my size? ────NO───→ SKIP (can't exit)
    ↓ YES
Time to resolution 45-780s? ──────NO───→ SKIP (too early/late)
    ↓ YES
Place ambush ladder (mid-1, mid-2, mid-3)
    ↓
Wait 1.5s for fill
    ↓
Fill? ──NO──→ Cancel all, log "no edge"
    ↓ YES
Cancel unfilled levels
Place post-only TP (entry + 2-4 ticks)
Start 6-12s time-stop
    ↓
Exit (TP fill / time-stop / reversal)
    ↓
Log P&L, cooldown 60s
    ↓
Return to ARMED
```

---

## 📈 Win Rate Drivers (What Actually Matters)

1. **Fill selectivity** - Only trade when edge is clear (30-50% fill rate is GOOD, not bad)
2. **Price improvement** - Mid-based ladder vs bestBid saves 1-3 ticks per trade
3. **Exit speed** - Post-only TP gets you queue position advantage
4. **Time discipline** - 6-12s hold prevents "stuck" positions eating into profit
5. **Adverse selection avoidance** - Skipping toxic flow prevents 70%+ of losing trades

---

## 🎓 Key Mindset Shifts

| Wrong Thinking | Right Thinking |
|----------------|----------------|
| "I need to get filled every time" | "I only want to get filled when I have edge" |
| "Place at bestBid for max fill rate" | "Place at mid-offsets for better selection" |
| "Hold to settlement for max profit" | "Harvest repricing wave, don't speculate" |
| "If not filled, requote higher" | "If not filled, there was no edge - skip" |
| "More trades = more profit" | "Better trades = more profit" |

---

This document should be the **decision framework** for the bot's maker logic. Every order placement should pass through these filters.
