# Repo Map

Auto-generated project map for fast code navigation and AI context.

## Files

- `README.md` — Project documentation.
- `deploy/sebessegbot.service` — Systemd service definition.
- `rust_bot/Cargo.toml` — Source/config file.
- `rust_bot/README.md` — Project documentation.
- `rust_bot/src/config.rs` — Source/config file.
  - symbols: `AppConfig, StrategyConfig, default_min_ladder_order_notional_usd, LadderLevel, SignalConfig, ZScoreConfig, RsiConfig, BollingerConfig`
- `rust_bot/src/logic/l1_gate.rs` — Source/config file.
  - symbols: `HyperliquidL1Gate, new, now_ms, run, nonce_strictly_increases`
- `rust_bot/src/logic/mod.rs` — Source/config file.
- `rust_bot/src/logic/order_manager.rs` — Order payload builder (ladder, TP/SL, close).
  - symbols: `LimitOrderType, TriggerOrderType, OrderTypeWire, OrderWire, OrderAction, CancelWire, CancelAction, UpdateLeverageAction`
- `rust_bot/src/logic/signal.rs` — Source/config file.
  - symbols: `SignalResult, update, evaluate, ZScoreIndicator, new, rolling_price_std, RsiIndicator, BollingerIndicator`
- `rust_bot/src/logic/signer.rs` — Source/config file.
  - symbols: `HyperliquidSigner, new, get_address, sign_l1_action`
- `rust_bot/src/main.rs` — Application entrypoint and runtime orchestration.
  - symbols: `main, ladder_gate_flat_ok`
- `rust_bot/src/network/client.rs` — REST client helpers and response guards.
  - symbols: `HyperliquidClient, new, clearinghouse_state_payload, get_meta, get_user_state, get_spot_clearinghouse_state, get_account_value_usd, get_frontend_open_orders`
- `rust_bot/src/network/feed.rs` — WebSocket feed handling and book/user events.
  - symbols: `L2BookState, default, FillEvent, WsRequest, SubscriptionData, WsResponse, HyperliquidFeed, new`
- `rust_bot/src/network/mod.rs` — Source/config file.
- `rust_bot/strategy_maker.json` — Configuration data.
- `scripts/README.md` — Project documentation.
- `scripts/repo_map.py` — Source/config file.
  - symbols: `is_text_file, should_skip, short_description, extract_symbols, collect_files, render_markdown, main`
- `setup.sh` — Operational helper script for build/run/deploy.
