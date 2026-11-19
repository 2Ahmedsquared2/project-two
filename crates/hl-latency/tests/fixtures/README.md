# Fixtures

Real frames from the public Hyperliquid WebSocket, captured 2026-11-19 from
`wss://api.hyperliquid.xyz/ws` (subscriptions `l2Book` BTC and `trades` BTC).
Trimmed by hand-run script, not edited: `l2Book` keeps the first level per
side, `trades` keeps the first trades and drops the `users` field. Used to
check the parser against the live schema, not invented examples.
