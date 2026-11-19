# Running the reference `hl-node` (non-validator) for Project Two

Status: plan, not yet run. No host exists; creating one needs Ahmed's approval
(cost table at the end). Everything below is from the official
[`hyperliquid-dex/node` README](https://github.com/hyperliquid-dex/node) (read
2026-11-19) unless marked INFERENCE.

## Why we need it

1. **Ground truth for the L4 checker.** `book-check` needs per-order states and
   oids; only a node writes them (`PHASE1_FINDINGS.md` §6-7).
2. **The oracle** our own node is diffed against, block by block.
3. **The latency baseline**: how much earlier a local node sees a block than
   the public WebSocket (`docs/LATENCY_PROBE.md`).

## Host

- Non-validator spec (README): 16 vCPU, 128 GB RAM, 500 GB SSD, Ubuntu 24.04
  only. Gossip ports 4001 and 4002 open to the public, otherwise peers
  deprioritise the node. README: "For lowest latency, run the node in Tokyo".
- Our placement rules (aws-project-two skill): AZ ID `apne1-az1`, PHC-capable
  current-gen instance, precision-time placement group, ENA Express off, IMDSv2,
  SSM instead of SSH, only 4001/4002 inbound, info server port 3001 never public.
- Output volume: ~100 GB/day with defaults (README); rotate or archive.

## Install (on the host)

```bash
echo '{"chain": "Mainnet"}' > ~/visor.json
curl https://binaries.hyperliquid.xyz/Mainnet/hl-visor > ~/hl-visor && chmod a+x ~/hl-visor
# verify: import pub_key.asc from the node repo, then
gpg --verify hl-visor.asc hl-visor
```

Mainnet non-validators need at least one seed IP in
`~/override_gossip_config.json`; get current ones from
`{"type":"gossipRootIps"}` on the public info API, plus the community root
peers the README lists (most are in Japan). Shape:

```json
{ "root_node_ips": [{"Ip": "<seed>"}], "try_new_peers": true, "chain": "Mainnet", "reserved_peer_ips": [] }
```

`split_client_blocks: true` in the same file streams uncommitted mempool
transactions to `~/hl/data/mempool_txs/{date}` (only if every peer between us
and the validators also sets it).

## Run

```bash
~/hl-visor run-non-validator \
  --write-order-statuses --write-raw-book-diffs --write-fills \
  --stream-with-block-info --disable-output-file-buffering \
  --serve-info
```

- `--write-order-statuses` / `--write-raw-book-diffs`: per-order data that
  rebuilds L4 and gives oids (the checker's missing ground truth).
- `--write-fills`: fills in the API format (settles fill price, self-trade rule).
- `--stream-with-block-info`: events written as processed, in the
  `{local_time, block_time, block_number, events}` schema; earlier than
  `--batch-by-block`, which waits for the block (README). This schema is what
  `hl-latency` parses.
- `--disable-output-file-buffering`: flush every line; lower latency, more IO.
- `--serve-info`: local `/info` on `localhost:3001` (subset of requests, no
  WebSocket).
- Live once logs show `applied block X`. Crash logs:
  `~/hl/data/visor_child_stderr/{date}/{node_binary_index}`.

## First-hour acceptance (INFERENCE: our checklist)

1. `applied block` lines advance; height within a few blocks of the public API.
2. `node_order_statuses`, `node_raw_book_diffs`, `node_fills` hourly files grow.
3. `hl-latency` probe runs for 1 h (see `docs/LATENCY_PROBE.md`).
4. One hour of node output converted into a dataset and checked by `book-check`
   (needs the node-output converter, next PR).
5. `ethtool -S` allowance counters captured before and after.
