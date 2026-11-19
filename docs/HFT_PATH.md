# HFT path: node output to strategy to order, on one host

Status: design, not yet measured. Written after Ahmed set the target to HFT:
tick-to-order latency and its tail (p99, p99.9) come first; throughput and
backtest speed come second. Nothing here has run on the Tokyo host yet.

## 1. What we can and cannot change

- `hl-node` is a closed-source binary. Its only documented outputs are files
  under `~/hl/data/` (see [NODE_SETUP.md](NODE_SETUP.md)), written line by line.
  `--disable-output-file-buffering` flushes each line. We know of no shared
  memory, socket or pipe output. To confirm on the host, run `hl-node --help`
  and ask Hyperliquid; until then treat files as the only interface.
- So the first hop is fixed for now: **hl-node writes, then we read**. Our own
  code controls every hop after that.
- The long-term way to remove the file hop is our own Rust node, which replays
  HyperCore itself and publishes straight into memory (Phase 3+). That is the
  real "faster than hl-node" path; everything below is the bridge to it.

## 2. Hop budget, fewest hops first

```
hl-node --(write+flush)--> page cache file --(read)--> reader thread
   --(SPSC ring in shared memory)--> strategy thread --(signed order)--> TLS socket --> HL API
```

| Hop | Owner | Floor | Lever |
|---|---|---|---|
| hl-node flush | hl-node | unknown; measured by LATENCY_PROBE as "time to flush" | `--disable-output-file-buffering`; put `~/hl/data` output on tmpfs if disk IO shows in the tail (test it: the files are large, so mind RAM) |
| detect new bytes | us | poll interval or inotify wake | busy-poll `read()` on a pinned, isolated core; inotify behind a flag; A/B both |
| parse line | us | microseconds of JSON | parse only the fields the strategy needs; no allocation (borrowed slices, fixed-point ints) |
| hand-off to strategy | us | ~100 ns cross-core on one socket | single-producer single-consumer ring, cache-line padded indices, fixed-size records, no locks, no syscalls |
| strategy decision | us | strategy dependent | same process as the ring consumer, no async runtime on this thread |
| sign + send order | us | EIP-712 sign plus one TLS write | pre-built request templates, keep-alive TLS connection opened and warmed in advance |
| network to HL API | AWS/HL | same-AZ round trip | host in `apne1-az1` (already approved scope) |

Two options for the reader to strategy hop, in order of preference:

1. **One process, two pinned threads, in-process SPSC ring.** Fewest hops; no
   shared-memory setup; a crash takes both down, which is acceptable for a
   strategy that must not trade on stale data anyway.
2. **Two processes, SPSC ring in `/dev/shm` (mmap).** Same ring code; lets the
   reader run independently of strategies and feed several of them (one ring
   per consumer, never a shared multi-consumer queue on the hot path).

Start with (1); move to (2) only if we need several strategy processes.

Record layout (draft): fixed 64-byte records, one cache line each:
`seq: u64, block_time_ns: u64, recv_ns: u64, coin: u32, kind: u8, side: u8,
px: i64 (fixed point), sz: i64 (fixed point), oid: u64, pad`. Gaps in `seq`
are a hard error the strategy must see; it stops quoting until resynced.

## 3. What to measure (answers Latency's item 2)

All on the Tokyo host, probe and reader on isolated cores, chrony offset and
`ethtool -S` allowance counters recorded before and after (as in
[LATENCY_PROBE.md](LATENCY_PROBE.md)).

1. **Detect latency, file tail:** a writer process appends stamped lines to a
   file (same flags and filesystem as hl-node), the reader stamps when it sees
   them. Modes: busy-poll, 10 us poll, 100 us poll, inotify. Report p50, p99,
   p99.9, max and sample count of (seen - written).
2. **Same file on tmpfs vs the EBS-backed data dir.**
3. **Hand-off:** writer to strategy thread through the in-process ring and
   through the `/dev/shm` ring; same percentiles.
4. **Real feed:** the node arm of `hl-capture` tailing hl-node's own files,
   A/B poll vs inotify on the same lines (Latency's request on PR #3).

Synthetic runs (1)-(3) can also run on any Linux box to test the harness, but
only host numbers count; this shared cloud sandbox is too noisy to quote.

## 4. Where orders are sent from (answers Latency's item 3)

- Orders leave **from the same Tokyo host**, from the strategy process itself,
  over a pre-opened TLS connection to the Hyperliquid API (`/exchange`).
  Non-validator nodes do not accept or forward user orders as far as we know;
  confirm with the Hyperliquid Reason.
- Tick-to-order is stamped in one process on one clock:
  `t0` = reader sees the line, `t1` = strategy decides, `t2` = signed bytes
  handed to `write()` on the socket. Report `t2 - t0` with p50 to p99.9.
  Optionally add NIC hardware TX timestamps (`SO_TIMESTAMPING`) later; r8i PHC
  and `ethtool -T` support are to be checked on the host.
- End-to-end including the exchange: match our order's ack/fill timestamp
  from the API back to `t0`; this mixes in HL's own processing, so report it
  separately from `t2 - t0`.
- **No real orders are placed** for any of this without Ahmed's explicit
  approval. Until then, measure `t2` against a local sink or a dry-run signer
  that builds and signs the bytes but does not send them.

## 5. Open questions

- Does `hl-node` have any non-file output or a flag we have missed? (Hyperliquid Reason)
- Is putting hl-node's output dir on tmpfs safe for disk/RAM over a week? (measure on host)
- Testnet vs mainnet for the first signed-order path test, and with which
  wallet? Ahmed's decision; nothing is sent until he approves.
