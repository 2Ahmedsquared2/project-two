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
`AV − IM − E + spot USDC ≥ raise`, with `AV = u + Σ szi·mark`,
`IM = Σ |szi|·mark ÷ lev`, and unset leverage = min(20, asset max).

Clean case: the user's first perp limit order in the window, before any fill
of theirs (maker or taker, from the node's fills file), cancel, modify or other
signed action; isolated-margin users excluded. Built by
`crates/hl-account/tools/build_margin_fixture.py`.

| Window | Cases | Match |
|---|---|---|
| 1232430001..1232432000 (fitted) | 206 | 204 |
| 1232432001..1232440000 (frozen) | 81 | 77 |

### Inferences, named

- **Spot USDC counts as perp collateral for everyone in the sample.** Users whose
  perp cash is exactly 1.01 × `e` (an automatic top-up) only fit with it. One
  miss (1232430014, `0xd839e870`) contradicts it: spot would cover, native
  rejects. The per-user mode flag has not been found in the state.
- **Default leverage 20.** Fixes the BTC case at 1232430063; 25 fits this sample
  equally. Margin tiers are not involved (BTC's second tier starts at $150M).
- **The 4 out-of-sample misses** are all native accepting what we reject;
  unchecked suspicion: funds arriving from other users or deposits, which the
  clean filter does not see.

### Not modelled yet

Isolated margin, margin tiers, builder dexes, fees and funding (no funding
boundary falls in the window), state evolution through fills and cancels, and
the unified-mode flag.
