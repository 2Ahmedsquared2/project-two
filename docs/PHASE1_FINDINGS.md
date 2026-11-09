# Phase 1 findings: how the Hyperliquid order book works, for a non-expert

Hyperliquid Reason, 2026-11-09. Companion to `ACTION_ORDERING.md` (the precise spec with sources) and
`dataset/README.md` (what we recorded). Labels as there: VERIFIED (documented), OBSERVED (measured by us),
INFERENCE (our reading), UNVERIFIED (nobody has shown it yet).

## 1. The picture in one paragraph

Hyperliquid is a blockchain whose main program, HyperCore, is an exchange. Every ~70 ms (OBSERVED) the validators agree on a
block: a list of signed requests from traders ("actions": place order, cancel, modify, change leverage, …). Every node then
executes that list in the same order and gets the same result, so every node holds an identical copy of every order book.
An order book per market is two sorted lists: bids (people wanting to buy, highest price first) and asks (people wanting to
sell, lowest price first). At each price there is a queue of orders, first come first served. A new order that is willing to
pay the other side's price trades immediately ("takes"); otherwise it waits in the queue ("rests"). Project Two re-implements
exactly this execution, so that our book equals the official node's book after every block. Phase 1 builds the yardstick:
a simple, obviously-correct reference book, a recorded stream of real blocks, and a checker that says where two books differ.

## 2. Vocabulary

- **Action**: one signed request. An `order` action can carry several orders; a `cancel` carries several cancels.
- **oid**: the integer id the exchange gives an order when it is accepted. Cancels refer to it. **cloid**: a client-chosen id.
- **Tick** and **lot**: the smallest price step and the smallest size step. All prices are whole numbers of ticks, all sizes whole numbers of lots. (VERIFIED)
- **L2 book**: per price level, the total size and the number of orders. **L4 book**: every individual order in queue order. The public API only serves L2; a node can write L4.
- **GTC / ALO / IOC**: how long an order may live. GTC rests until filled or cancelled. ALO ("post only") must rest; if it would trade immediately it is rejected. IOC must trade immediately; whatever is left is thrown away. (VERIFIED)
- **Trigger orders** (stop, take-profit): not on the book until a price condition fires. (VERIFIED)

## 3. The surprising rule: a block is not executed in arrival order

Most chains execute transactions in the order the block proposer saw them. HyperCore sorts them first (VERIFIED, `hypercore/order-book`):
within each consensus batch (a block has one or several), it runs

1. everything that does **not** place a GTC or IOC order (so: ALO orders, leverage changes, transfers, …),
2. then all **cancels**,
3. then everything that places a GTC or IOC order.

Inside each group the proposer's order is kept. The point is fairness for market makers: their quote updates (ALO) and their
cancels always land before anyone's aggressive order in the same batch, so they are not run over. We saw exactly this
pattern in all 1,242 recorded blocks (OBSERVED): the transaction list of each block is one to eight back-to-back runs of
"group 1, group 2, group 3".

There is a second layer, priority fees (VERIFIED, `api/priority-fees`): a taker can pay to move earlier in the mempool
(up to ~45 ms per basis point, 0-8 bps), and a maker can pay to be placed ahead of other makers who joined the same price
level in the last 400 ms. Cancels are always first regardless of anyone's fee. For our replica none of this needs computing:
the recorded block already is the result; we only have to replay it in the recorded order.

## 4. Matching, in plain terms

(VERIFIED unless noted) Price-time priority: the best price trades first; at equal price, the earlier order trades first.
An incoming buy at price P trades against asks priced ≤ P, cheapest first, oldest first, each trade at the resting order's
price (INFERENCE, standard for every CLOB and consistent with the `avgPx` the API returns). A partially filled resting order
shrinks in place and keeps its place in the queue (INFERENCE from the node's `update` diff). If a trader would trade with
their own resting order, that resting order is cancelled instead and nothing trades (VERIFIED, self-trade prevention).
Modifying an order is a cancel plus a brand-new order, so it goes to the back of the queue (INFERENCE from the modify docs).

## 5. Numbers: why we never use floating point

Prices may have at most 5 significant figures and at most 6 − szDecimals decimals (perps) or 8 − szDecimals (spot); whole
numbers are always allowed. Sizes have at most szDecimals decimals. (VERIFIED) Every valid price and size is therefore an exact
integer once multiplied by a per-asset power of ten, so the reference book stores `px_ticks` and `sz_lots` as integers and compares
integers. A floating-point `74668.0` is not reliably equal to another `74668.0` after arithmetic; an integer `746680` is.

## 6. What we recorded and what it proves

From the public explorer and API, on 2026-11-09 11:35-11:45 UTC (OBSERVED):
- 1,242 consecutive blocks (86 s of chain time, 2.4 M transactions): every action of every block, in execution order; 5,974 transactions
  carried an error (mostly "already expired" or EVM nonce errors) and changed nothing.
- 253 L2 snapshots for 10 markets, each pinned to an exact block (the snapshot's `time` equals the block's `blockTime`, 253 of 253).
- Mix: 1.13 M order actions (97 % ALO: market makers quoting), 1.19 M cancels (78 % by client id), 58 k modifies.

What it does **not** contain, because the official node could not run here (no raw-TCP gossip egress from the sandbox; the
binary needs a newer glibc): the oid of each new order, individual fills, and the per-order (L4) book. Without a seed book and
oids, a replay from public data cannot be checked for equality against the real book. The checker's correctness is instead proven
on hand-built golden vectors (a deliberately corrupted book is caught at the exact block, side, price and field), and the dataset
format is proven by ingesting the real 1,242-block stream.

## 7. Open questions, who owns them, how they get answered

| Question | Why it matters | How to settle |
|---|---|---|
| Exact oid assignment (observed ~1.035 oids per visible order leg) | plain `cancel` resolution in replay | node `node_order_statuses` files |
| Explorer order == node execution order | the whole replay | one block vs `replica_cmds` (S3 `hl-mainnet-node-data`, requester-pays, cents) or a node |
| Self-trade "same address" = master account or signer? | a wrong rule cancels the wrong order | node fills + order statuses for a sub-account trader |
| Reduce-only clipping, min-notional threshold, fill price rule | edge cases in matching | node fills |

The one investment that answers all four: an Ubuntu 24.04 host with ports 4001/4002 open running `hl-visor run-non-validator
--write-order-statuses --write-raw-book-diffs --write-fills --serve-info` for an hour (README of `hyperliquid-dex/node` says
16 vCPU / 128 GB RAM / 500 GB SSD for a non-validator; disk output ~100 GB/day). That is a product/budget decision for Ahmed via the CTO.

## 8. TL;DR

Blocks are sorted (non-taker actions, cancels, takers) per batch, then replayed identically everywhere; matching is price-time
with self-trade cancelling the maker; prices and sizes are exact integers. We recorded 1,242 real blocks plus exact-height L2
snapshots and wrote the spec; the per-order ground truth needs a real node host.
