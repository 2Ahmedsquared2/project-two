# ACTION_ORDERING.md: how HyperCore orders and matches actions in a block

Spec for `hl-book-ref` (Project Two, Phase 1). Written by the Hyperliquid Reason, 2026-11-09.
Every rule carries a label: **VERIFIED** = stated in the cited primary source; **OBSERVED** = measured in our
public-data capture (`dataset/README.md`), with the sample size; **INFERENCE** = my reading;
**UNVERIFIED** = not documented, not measured; needs node data or an experiment. Do not implement an
UNVERIFIED rule as fact; implement it behind a flag or leave a TODO that names it.

Source keys (all docs are `https://hyperliquid.gitbook.io/hyperliquid-docs/<path>`; append `.md` for Markdown):
- [OB] `hypercore/order-book`
- [PRIO] `for-developers/api/priority-fees`
- [LAT] `for-developers/api/optimizing-latency`
- [TICK] `for-developers/api/tick-and-lot-size`
- [NONCE] `for-developers/api/nonces-and-api-wallets`
- [EXCH] `for-developers/api/exchange-endpoint`
- [TYPES] `trading/order-types`
- [STP] `trading/self-trade-prevention`
- [ASSET] `for-developers/api/asset-ids`
- [SCHEMA] `for-developers/nodes/l1-data-schemas`
- [STAKE] `hypercore/staking`
- [OBS] `https://github.com/hyperliquid-dex/order_book_server` (official example consumer; not maintained)

---

## 1. Block structure and the three action groups

1.1 **VERIFIED** [OB] A block contains one or more *consensus batches* ("usually 1-2"). Within each batch, actions are sorted into three groups, executed in this order:
   1. actions that do **not** send a GTC or IOC order to any book (ALO-only order actions, cancels are not here, leverage changes, transfers, noops, EVM txs, …);
   2. **cancels**;
   3. actions that send **at least one GTC or IOC** order.

   Within a group, actions keep the order in which the block proposer proposed them. A `modify`/`batchModify` is categorised by the **new** order it places.
   [LAT] states the same in short: "within a bundle, ALO and cancels execute before other txs".

1.2 **OBSERVED** (1,242 blocks, 2.41 M txs) The explorer `blockDetails.txs` list decomposes into 1-8 consecutive non-decreasing runs of the pattern 1→2→3 (204 blocks had 1 run, 409 had 2, 379 had 3, 250 had 4-8). Consistent with "concatenation of sorted batches". Batch boundaries are **not** exposed.
   **INFERENCE** The explorer order is the execution order; replay `txs` in file order. **UNVERIFIED** against `replica_cmds` (needs a node).

1.3 **VERIFIED** [PRIO] Mempool/proposal order is by `effective_time = arrival_time + f(action, priority_fee)`, `f` strictly decreasing in priority fee and **zero for cancels**. "Even with order priority, all cancels are prioritized before all immediately executable orders." IOC with priority ≥ 8 bps: within each 70 ms unix-time bucket ("usually 1 block") sorted by decreasing fee. Block boundaries do not matter for relative order of a given action type.
   For the reference book this is **not** something to compute: the recorded order already embeds it. Keep it in mind only to explain why blocks look the way they do.

1.4 **VERIFIED** [STAKE] Height increments only for rounds containing at least one transaction. **OBSERVED** mean block interval 69.6 ms over 1,242 blocks (min ~50 ms); `blockTime` is unique per block and equals each `txs[].time`.

1.5 **VERIFIED** [EXCH] Actions may carry `expiresAfter` (ms); a stale action is rejected and still appears in the block with `error` (OBSERVED 1,443 × `"Action already expired"`). **INFERENCE** errored txs change no book state. **UNVERIFIED** whether they consume an oid (see §5).

## 2. Within an order action

2.1 **VERIFIED** [EXCH] An `order` action carries `orders[]` (each `a` asset, `b` isBuy, `p` price, `s` size, `r` reduceOnly, `t` type, `c` cloid), `grouping` (`"na" | "normalTpsl" | "positionTpsl"`) and optional `builder`. The response has one status per order, in order: `resting{oid}`, `filled{totalSz, avgPx, oid}` or `error{string}`.
   **INFERENCE** orders inside one action are applied sequentially in array order (the statuses array is positional). **UNVERIFIED** by a primary statement.

2.2 **VERIFIED** [PRIO] `grouping` may instead be `{"p": N}`: an order-priority fee rate `N / 1e8`, allowed only when every order is IOC, or every order is a non-reduce-only ALO, on non-outcome assets. OBSERVED in 35 % of order actions in our capture.

2.3 **VERIFIED** [TYPES][EXCH] Time-in-force semantics:
   - **GTC**: rests until filled or cancelled; may match first.
   - **ALO** (post only): "canceled instead of immediately matching" [EXCH]; it never takes. Rests if it does not cross.
   - **IOC**: fills what it can immediately; the remainder is cancelled; never rests.
   - Trigger orders (`t.trigger{isMarket, triggerPx, tpsl}`): not on the book until triggered; TP/SL "are automatically market orders" [TYPES]. **OBSERVED** `tif: "FrontendMarket"` (269 orders) appears in blocks; the API docs show it only in order-status responses. **INFERENCE** it is an IOC market order with the frontend's slippage limit.
   - **Reduce only** (`r: true`): only reduces an existing position. **UNVERIFIED** exact handling when it would increase (rejected vs. size-clipped); needs node data.

2.4 **VERIFIED** [OB] Margin checks happen when an order is opened and again on the resting side at each match. **INFERENCE** a match can therefore remove or shrink a resting order without a cancel. Out of Phase 1 scope: the checker must report such divergences, not crash.

## 3. Matching and queue priority

3.1 **VERIFIED** [OB] "Orders are matched in price-time priority." Prices are integer multiples of tick size, sizes integer multiples of lot size.

3.2 **VERIFIED** [SCHEMA][OBS] Within a price level the queue is a list; the L4 snapshot lists orders "time-ordered within a level" with trigger orders at the end. A new resting order is appended at the back, unless `insertBefore` is given.

3.3 **VERIFIED** [PRIO] **ALO priority** reorders the tail of a level: with T = 400 ms, orders placed within the past T form the tail; a new ALO is inserted before tail orders with a lower priority rate (the example `B > A > C`). An order older than T has locked its place. Node raw book diffs express this as `{"new": {"sz", "insertBefore": <oid>}}` [SCHEMA][OBS].
   For Phase 1: implement `insert_at_back` and `insert_before(oid)`; computing *where* from timestamps and `{"p": N}` is Phase 2 unless the dataset supplies `insertBefore`.

3.4 **VERIFIED** [STP] Self-trade: "Trades between the same address cancel the resting order instead of causing a fill. No fees are deducted, nor does the cancel show up in the trade feed." So when an incoming order from account X would match a resting order of X, the resting order is removed and the incoming order continues matching. **UNVERIFIED** whether "address" means the master account (so sub-accounts/vaults count as the same) or the exact signing account.

3.5 **INFERENCE** (standard CLOB, consistent with [OB] and the `filled{avgPx}` status) An incoming order matches the opposite best level first, FIFO within a level, each fill at the **resting** order's price, until the incoming price no longer crosses or its size is exhausted; a GTC remainder rests, an IOC remainder is dropped. **UNVERIFIED** by a primary statement; the dataset's fills (when we have node data) will confirm.

3.6 **VERIFIED** [EXCH] An order below the minimum notional is rejected (`"Order must have minimum value of $10."`). **UNVERIFIED** exact threshold per asset and whether it is checked on remaining size.

## 4. Tick and lot size, integer representation

4.1 **VERIFIED** [TICK]
   - Price: at most **5 significant figures**, and no more than `MAX_DECIMALS − szDecimals` decimal places, `MAX_DECIMALS = 6` for perps, `8` for spot. **Integer prices are always valid** regardless of significant figures (`123456` valid, `12345.6` not).
   - Size: rounded to `szDecimals` of the asset (`szDecimals = 3` → `1.001` valid, `1.0001` not).
   - `szDecimals` comes from `meta` / `spotMeta` (spot: the base token's `szDecimals`).
   - Signing strips trailing zeros from the number strings.

4.2 **INFERENCE** Integer encodings for the reference book: `px_ticks = px × 10^(MAX_DECIMALS − szDecimals)` and `sz_lots = sz × 10^szDecimals`; both are exact integers for every valid order, so parse decimal strings to integers, never through f64. The L2 level `n` is an integer order count.

4.3 **VERIFIED** [ASSET] Asset ids: perp = index in `meta.universe` (BTC = 0); spot = `10000 + spotMeta.universe[].index`; builder perps = `100000 + perp_dex_index × 10000 + index_in_meta`; outcomes = `10 × outcome + side`.

4.4 **VERIFIED** [OB] "Orders are added where price is an integer multiple of the tick size, and size is an integer multiple of lot size." **INFERENCE** tick size for a given price is `10^−(decimals allowed by the 5-sig-fig rule)`; it changes with price magnitude, which is why the book must key on the integer `px_ticks` at the asset's maximum resolution, not on a fixed tick.

4.5 **VERIFIED** [EXCH] A non-errored action can still reject individual orders: the exchange response is `"status":"ok"` with a positional `statuses` array whose entries may be `{"error": "..."}` (the docs' own example is the $10 minimum-value error). The explorer's tx-level `error` field only reports action-level failure. **OBSERVED** in the Phase 1 dataset: 1,240 of ~865k order/modify legs in txs with `error: null` break 4.1 (e.g. BTC `74653.7` = 6 significant figures; spot `74.19981144` = more than `8 − szDecimals` decimals; BTC size `0.000034` > 5 szDecimals). **INFERENCE** these are per-order rejections: the replay must reject the leg, leave the book unchanged and continue with the next leg. **UNVERIFIED** whether a rejected leg consumes an oid (see 5.2). This does not change rule 4.1.

## 5. Order ids (oid)

5.1 **VERIFIED** [EXCH] An accepted order receives an integer `oid` returned in `resting`/`filled` statuses; cancels reference `oid` (or `cloid` via `cancelByCloid`); `modify` references `oid` (or cloid).

5.2 **OBSERVED** oids form a single global, strictly increasing counter (5 orders located via `orderStatus` across a 1,000-block window; oid 4.3e11 at height 1e9, 5.99e11 at height 1.21e9). Between anchor blocks the oid delta exceeded the count of order and modify legs in `blockDetails` by 3.4-3.5 %.
   **UNVERIFIED** the exact assignment rule and what else consumes oids (candidates: CoreWriter orders from `evmRawTx`, TWAP sub-orders, liquidation/ADL orders, triggered-order activations, per-order rejections inside a non-errored action). The node's `node_order_statuses` files settle this; until then a replay from public data can only resolve `cancelByCloid` (by user + cloid) exactly, and must treat plain `cancel` resolution as approximate.

## 6. Nonces (validity, not ordering)

6.1 **VERIFIED** [NONCE] Per **signer** (the user address, or the API/agent wallet address), the chain stores the **100 highest nonces**. A new transaction's nonce must be larger than the smallest nonce in that set and must never have been used before. Nonces must lie within `(T − 2 days, T + 1 day)` where `T` is the block's unix ms timestamp. API wallets and their nonce sets can be pruned (deregistration, expiry, zero funds), after which old signed actions become replayable: do not reuse agent addresses.

6.2 **INFERENCE** Nonce checks are a validity filter before execution; they do not affect order among valid actions. Phase 1 ignores nonces: the dataset contains only committed transactions. **UNVERIFIED** whether a nonce-rejected action appears in a block with an `error` or is dropped before inclusion.

## 7. What the reference book must therefore do (Phase 1)

1. Keep one book per asset; prices as `px_ticks`, sizes as `sz_lots` (integers, §4.2).
2. Replay a block's `txs` in file order (§1.2); skip txs with `error != null`.
3. For each order leg, in array order (§2.1): trigger order → record, do not touch the book; ALO → rest at back (or `insert_before`) if it does not cross, else drop (§2.3, §3.3); GTC → match (§3.5) then rest remainder; IOC/FrontendMarket → match then drop remainder; apply self-trade rule (§3.4).
4. `cancel` by oid, `cancelByCloid` by (user, cloid), `modify` = remove old then place new (§1.1, §5).
5. Expose L2 aggregation (level → total `sz_lots`, count `n`) for comparison with `l2_anchored.ndjson`, and the L4 queue for comparison with node L4 data later.
6. Treat margin-driven removals, liquidations, funding and ADL as out of scope; report divergences they cause.

## 8. Open questions I will chase (owner: Hyperliquid Reason)

- oid assignment rule (§5.2) — needs `node_order_statuses`; a node host is required (see `dataset/README.md`).
- Explorer order == execution order (§1.2) — compare one block against `replica_cmds` from S3 `hl-mainnet-node-data` (requester-pays; needs AWS credentials; cost is cents) or a node.
- Self-trade "address" scope (§3.4); reduce-only clipping (§2.3); minimum notional (§3.6); fill-price rule (§3.5).
