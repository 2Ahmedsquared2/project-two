# Perp cross-margin check (hl-account): what is read from data, what is inferred

All numbers are from **our node**: blocks 1232430001..1232440000, seeded from
our node's translated ABCI state at 1232430000 (AWS key
`window/our-node/translated/1232430000.json.zst`, sha256 `b8c6b414…`). Native's
responses are the answer key only; no decision reads them.

## State fields used (from data)

Main perp dex is `/exchange/locus/cls[0]`; spot is `/exchange/locus/scl`.

| Need | Path | Units |
|---|---|---|
| perp cash | `cls[0].user_states.user_to_state[addr].u` | micro-USDC |
| positions | `….p.p[[asset, {s, e, l, M, f}]]` | `s` signed lots, `e` entry notional micro-USDC |
| leverage setting | `….p.p[asset].l` = `{C: lev}` or `{I: {l, u}}` | absent = never set |
| resting-order margin | `cls[0].user_states.open_order_tracker.user_to_data[addr]` = `{e, a: [[asset, {n:[bid,ask], s:[bid,ask], P, p, l}]]}` | `n` = Σ px_ticks·lots |
| spot USDC | `scl.user_states[addr].b[[0, {t, h}]]` | 1e-8 |
| collateral mode | `exchange/locus/ust[addr].a` = `u` (unified), `i`, `p` (portfolio); absent = unset | set by `userSetAbstraction` / `agentSetAbstraction` |
| max leverage | `cls[0].meta.marginTableIdToMarginTable` (ids < 50 are the plain max leverage) | |
| mark | `cls[0].oracle.pxs2[asset][1].p` at the seed; `SetGlobalAction.pxs[asset][1]` after | ticks |

## The stored resting-order margin `e` (from data)

Per asset, the side that grows the position pays its full notional; the side
that shrinks it pays only for lots beyond `2·|szi|` (pro rata); the asset charges
the larger side ÷ the orders' leverage, floored; `e` is the sum.

| Snapshot | Users | Exact | Inexact |
|---|---|---|---|
| 1232430000 (fitted) | 13,710 | 12,613 | 1,097 |
| 1232440000 (frozen rule) | 13,694 | 12,588 | 1,106 |

Every inexact user has `e` *below* the recompute. About 820 of them per
snapshot are within 1e-5 relative, a one-signed drift we read as native keeping
`e` as a running total (inference). About 170 per snapshot are off by more than
0.1%; all of those have a shrinking side partly beyond `2·|szi|`, where neither
pro rata nor any per-order ordering (best, worst, oldest, newest price first)
reproduces native. That case is open.

## The placement check (fitted, then scored out of sample)

Accept iff the order does not raise its asset's charge, or
`AV − IM − E (+ free spot USDC when ust.a = u) ≥ raise`, with `AV = u + Σ szi·mark`,
`IM = Σ |szi|·mark ÷ lev`, and unset leverage = min(20, asset max).

Clean case: the user's first perp limit order in the window, before any fill
of theirs (maker or taker, from the node's fills file), cancel, modify or other
signed action; isolated-margin users excluded. Built by
`crates/hl-account/tools/build_margin_fixture.py`.

| Window | Cases | Match |
|---|---|---|
| 1232430001..1232432000 (fitted) | 206 | 205 |
| 1232432001..1232440000 (frozen) | 81 | 77 |

The 5 misses: 4 portfolio-margin accounts (`ust.a = p`, not modelled; native
accepts all four; the first is 0.51 USDC short by our rule, the three later ones
6,511.79, 2,060.62 and 373.50 in the evolved state) and 1 unified account 95.90
USDC short (1232438533, `0x89bec994`), open. No non-order action touches any of the 4 out-of-sample users between the seed and the order, so
"funds arriving" is refuted for them (read).

### Collateral mode (read field, rule inferred)

Spot USDC is perp collateral only when `ust.a = "u"`. Over every main-perp leg
whose verdict changes with spot USDC (blocks 1232430001..1232440000, evolved
state below): 318 of 318 in-sample and 1,218 of 1,218 out-of-sample match with
this rule; counting spot for everyone gets 47 + 118 of them wrong. It settles the
earlier contradiction (1232430014, `0xd839e870` is `i`). Default leverage stays
20 (25 is not separated by this data); margin tiers start at $150M (BTC) and are
not involved.

## Evolving state, per leg over all legs (`tools/evolve_margin.py`)

Seed: the state at 1232430000 for the 6,585 users that act or fill in the
window, plus the seed resting orders (`l4so_1232430000`). Per block, bundles run
in Hyperliquid's three-class order (no GTC/IOC leg, then cancels, then GTC/IOC
legs; lane 1's `exec_class`). Before each main-perp order, modify and
batchModify leg the check is decided from the evolved state; then the leg's
effects are applied from native's per-leg response (resting oid, filled) and the
fills stream (taker oid + tid), which stand in for our engine's output (read as
state inputs, never as the verdict). Fills with no leg (TWAP, liquidations,
triggers) apply at the end of the block; in the first 2,000 blocks the
order-status stream supplies the new oid of a single `modify` and cancels with no
action, which the later 8,000 blocks lack.

Legs where native answered accept or "Insufficient margin", modelled accounts
(cross, unified or independent, no unmodelled transfer, not reduce-only or
trigger). Labels: per-leg verdict matched; end-state check run on orders and
positions, see below.

| Blocks | 1 native margin, we margin | 2 native accept, we accept | 3 we reject, native accepts | 4 native margin, we accept | blocks where every such leg matches |
|---|---|---|---|---|---|
| 1232430001..1232432000 (fitted) | 28,620 | 156,215 | 12 | 37 | 1,783 of 1,812 |
| 1232432001..1232440000 (frozen) | 123,860 | 716,951 | 120 | 4,034 | 6,643 of 7,373 |

Not in the table (counted separately): users with no `ust.a` (114,573 + 444,949
legs, 0 + 3 in class 3/4), portfolio margin (94 + 251 legs, 11 + 41 in class 3),
isolated (84,915 + 381,810), users after an unmodelled transfer (13,614 +
87,546), reduce-only and trigger legs.

Class 4 out of sample is 25 accounts, mostly BTC/ETH, rising with time. Their
resting orders, cash and positions at 1232440000 equal the end state exactly, so
it is not state drift; one dump (`0x882a39dc`, short BTC at 2x with resting bids
inside `2·|szi|` and asks on the growing side) points at the two-sided
resting-order charge. Open.

### End-state check at 1232440000 (read)

- Resting main-perp orders: per (user, asset) aggregate, 32,056 of 32,493 equal;
  oids: native 223,468, ours 224,076, both 222,983.
- Positions: equal for 6,354 of 6,356 users; our position equals the fill's
  `startPosition` on 27,830 of 27,900 main-perp fills. `startPosition` is written
  via a float and must be rounded to the lot (`-1110074.1000000001`).
- Cash: of 703 clean cross users with only main-perp fills and no other action,
  `u -= signed lots × px + fee + builderFee` reproduces `u` exactly for 215 of
  223 `i` accounts; for `u` accounts, 22 exactly and 219 more only as perp cash +
  spot USDC (native moves USDC between the two by itself; inferred), 128 neither
  (median −0.02 USDC, p10 −10.60; open, HIP-3 balances suspected); `p` 0 of 42.
  `tests/evolve.rs` pins 4 of the exact accounts (up to 981 fills each).

### Execution order (read)

A `cancelByCloid` that precedes its ALO target in a bundle succeeds (92,570
pairs in the first 2,000 blocks) and one that precedes a GTC target never does
(14 of 14): the documented three-class order, which lane 1's engine already
applies per bundle.

### Not modelled yet

Isolated and portfolio margin, open-interest caps, builder dexes, funding (no
boundary in the window), the 128 unified cash residuals, the two-sided order
charge behind class 4, and the ~170 users per snapshot whose stored `e` is off by
more than 0.1%.
