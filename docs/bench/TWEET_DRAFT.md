# Draft post (for Ahmed to send himself; states only what REPORT.md and docs/proofs show)

Building a Hyperliquid node in Rust, clean-room, checked block by block
against the official node.

Measured: decoded 2,000 real mainnet blocks from our own node (1.86M actions,
42 action types) with zero errors; 99.3% of actions are order-book actions,
but only ~5% of blocks are pure order book, so full parity needs margin and
balance state too.

Proved: our warm-up gate never lets a measurement start before every warm-up
step passed and the window elapsed (Kani, bounded: up to 3 steps, 6 events;
6/6 planted bugs caught; it found one real bug, now fixed).

Not yet claimed: any speed comparison with the official node. That comes only
from a same-machine run where our output matches native's on every block.
