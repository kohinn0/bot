# Crypto Sebesseg Bot - File Structure

## 📁 Directory Contents

```
crypo_sebesseg/
├── .env                              # API keys (copied from parent)
│
├── Configuration Files
│   ├── strategy_maker.json           # Main maker-only strategy config
│   ├── strategy_taker.json           # Alternative taker strategy (for testing)
│   ├── strategy_config.json          # Original config (legacy)
│   ├── config.py                     # Python config loader with utilities
│   └── requirements.txt              # Python dependencies
│
├── Documentation
│   ├── MAKER_STRATEGY_GUIDE.md       # Professional maker techniques
│   ├── TAKER_FEE_IMPACT.md           # Fee analysis & maker rebate program
│   ├── FEE_INTEGRATION_QUICKSTART.md # Quick-start for fee integration
│   └── README.md                     # This file
│
└── Utilities
    ├── fee_utils.py                  # Fee-rate fetching & integration
    └── order_signing_example.py      # Complete order signing examples
```

## 🎯 Quick Start

### 1. Install Dependencies

```bash
pip install -r requirements.txt
```

### 2. Test Configuration

```bash
python config.py
```

Expected output:
- Strategy name and type
- Ladder configuration
- Take profit calculations
- Time-based scaling parameters

### 3. Test Fee Integration

```bash
python fee_utils.py
```

### 4. Review Strategy Documentation

Start with:
1. `MAKER_STRATEGY_GUIDE.md` - Core strategy principles
2. `TAKER_FEE_IMPACT.md` - Why maker-only is best
3. `FEE_INTEGRATION_QUICKSTART.md` - Implementation details

## 📊 Strategy Overview

**Type:** Maker-Only Ambush Ladder  
**Goal:** Be the trap, not the hunter

### Entry
- 3-level post-only bid ladder (mid-1, mid-2, mid-3 ticks)
- Wait 1.5s for panic sellers
- If no fill → cancel all (no edge)

### Exit
- Immediate post-only take-profit (entry + 2-4 ticks)
- Time-stop: 6-12s max hold
- Reversal stop: Close if Binance retraces 50%+

### Risk
- Max 1 position
- $20 max per trade
- $25 daily loss limit
- 60s cooldown between trades

## 🔑 Key Features

✅ **Maker-only discipline** - Never pay taker fees  
✅ **Toxic flow protection** - Skip trades when Binance shows sustained momentum  
✅ **Dynamic profit targets** - Adjust based on spread, depth, time-to-resolution  
✅ **Adverse selection avoidance** - Mid-price ladder for better fills  
✅ **Maker rebate earnings** - Get paid for providing liquidity

## 🚀 Next Steps

1. **Implement core components:**
   - Binance WebSocket feed
   - Polymarket orderbook reader
   - Signal engine
   - Order manager

2. **Build state machine:**
   - IDLE → ARMED → LADDER_PLACED → IN_POSITION → EXITING → COOLDOWN

3. **Test in dry-run mode:**
   - Verify Binance connection
   - Test market discovery
   - Validate order logic (without placing real orders)

4. **Live testing:**
   - Start with small position sizes
   - Monitor fill rates and P&L
   - Iterate on parameters

## 📚 Documentation Links

- [Implementation Plan](../../../.gemini/antigravity/brain/.../implementation_plan.md)
- [Task Checklist](../../../.gemini/antigravity/brain/.../task.md)

## ⚠️ Important Notes

- **Dry-run by default:** Set `DRY_RUN=false` in `.env` for live trading
- **Fee-rate required:** All orders must include `feeRateBps` (Jan 2026+)
- **Maker-only:** Never use market orders (taker fees are 1.6-3.7%)
- **Low fill rate is OK:** 30-50% fill rate means good selectivity

## 🔗 Key Files to Review

1. `strategy_maker.json` - All strategy parameters
2. `config.py` - Configuration loader and utilities
3. `MAKER_STRATEGY_GUIDE.md` - Strategy theory and best practices
4. `fee_utils.py` - Fee integration (critical for Jan 2026+)
