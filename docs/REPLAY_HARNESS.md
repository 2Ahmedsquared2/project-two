# Replay harness (`hl-replay`)

One process, one clock, one line at a time:

```
feeder --> reader --> parser --> book --> decision --> signer --> send --> loopback sink
 feed        t0         t1                                          t2
```

| Stamp | Taken when |
|---|---|
| `feed` | the feeder, just before it writes the line (open loop: one line every `--interval-us`, never waiting for an answer) |
| `t0` | the hot thread's `read` returned the line's bytes (line seen) |
| `t1` | the parser returned |
| `t2` | the write of the signed order to the sink socket returned (bytes handed to the socket) |

Four HDR histograms per arm, nanoseconds, 3 significant digits, 1 ns to 60 s:
`t2-t0` (the headline), `t1-t0`, `feed-t0` and `feed-t2`. `feed-t0` is where
the arms differ most: it holds the runtime's or the file poller's wake-up
delay, which `t2-t0` by definition does not. The report prints the sample
count, p50, p90, p99, p99.9, p99.99 and max. It prints no mean. Rounds are
merged histograms, never averaged percentiles.

## Rerun on the Tokyo host

```bash
git rev-parse HEAD                       # must match the report's `commit:` line, with no -dirty
cargo build --release -p hl-replay
./target/release/hl-replay \
  --arms all --rounds 5 --lines 11000 --warmup 1000 --interval-us 1000 \
  --feeder-core 2 --hot-core 3 \
  --out results/replay-$(date -u +%Y%m%dT%H%M%SZ)
```

Set `--feeder-core` and `--hot-core` to two different physical cores, the hot
one isolated if the host has isolated cores. To replay recorded node output
instead of generated lines, add `--input <file>` (one block-info JSON line per
line; the file is cycled). Send Latency the whole `--out` directory.

`--out` holds `report.txt` and, per arm and metric, `<arm>.<metric>.hdr` (HDR
V2 binary: mergeable, readable by any HdrHistogram library and by
`LatencyHist::read_v2`) and `<arm>.<metric>.buckets.txt` (`value_ns count` per
line, ending with a comment naming arm, metric, parser mode and commit).

## Arms

Runtime arms (`--arms runtime`, the default): lines arrive on a loopback TCP
socket.

| Arm | What it is |
|---|---|
| `busy-poll` | pinned thread spinning on a non-blocking socket |
| `tokio-current-thread` | tokio `current_thread`, pinned |
| `tokio-multi-thread` | tokio multi-thread, 2 workers, task spawned, never pinned: the stock setup |
| `compio` | compio 0.19.2 on `io_uring`, pinned |
| `compio-sqpoll` | the same with a kernel submission-queue polling thread |

Detect arms (`--arms detect`): lines are appended to a file in `--tail-dir`
(default `/dev/shm`) and the reader tails it, as the node's output is read.

| Arm | When the file has nothing new |
|---|---|
| `file-busy-poll` | spin |
| `file-poll-10us` | sleep 10 µs |
| `file-poll-100us` | sleep 100 µs |
| `file-inotify` | sleep on inotify `IN_MODIFY` |

A round runs every arm once; `--rounds N` repeats that, so arm A's round 2
comes after arm B's round 1 on the same machine under the same load. If
`io_uring` cannot be set up (seccomp, container policy, old kernel) the two
compio arms print `unavailable: <reason>` and the run carries on.

compio is behind the cargo feature `compio`, on by default. If it ever fails
to build on a host: `cargo build --release -p hl-replay --no-default-features`
builds everything else and those two arms report `unavailable`.

## What is real

The report prints this table from the stage types themselves:

| Stage | Label | Today |
|---|---|---|
| reader | real | `LineBuf`: fixed buffer, `read` straight into it |
| parser | real, mode `strict` | `hl_latency::parse_node_line` (builds a `serde_json::Value` tree) |
| book | stub | fixed 16-level arrays; prices derived from the block number, not from the line |
| decision | stub | one ALO order per update |
| signer | prototype | msgpack + 3x keccak-256 + libsecp256k1 + JSON body; placeholder domain, **not checked against SDK vectors** |
| send | prototype | plain TCP write to loopback: no TLS, no WebSocket framing, no NIC |

So `t2-t0` today is an upper bound on parsing plus a plausible signing cost.
It is not tick-to-trade. Every row is tagged `parser=strict` or
`parser=header scan` so a header scan's cost is never read as a full parse.
A header-scan parser is for stamping and routing only: a decision stage that
reads prices or sizes must take them from a strict parse.

### Swapping a stage

Stages are type parameters of `Pipeline` (static dispatch). The active ones
are four aliases in `crates/hl-replay/src/stage.rs`:

```rust
pub type ActiveParser = NodeLineParser;
pub type ActiveBook = StubBook;
pub type ActiveDecision = StubDecision;
pub type ActiveSigner = PrototypeSigner; // <- the one line to change
```

The zero-allocation `parse_node_line` keeps its signature, so it needs no
change here at all. For the real signer: implement `Stage` (with
`LABEL = StageLabel::Real` once it passes the SDK vectors) and `Signer` for
it, give it a `throwaway()` constructor, and change the alias. The report's
`signer=` label follows from the type.

## Allocation gates

`cargo test -p hl-replay --test alloc_gate -- --nocapture`

Each stage alone and the whole busy-poll tick (line written to the source
socket, read, parsed, booked, decided, signed, written to the sink socket,
both latencies recorded) run under `hl_alloc_gate::assert_no_alloc_after_warmup`.

| Gate | Allocations per tick after warmup |
|---|---|
| reader, book, decision, signer, send, clock + histogram record | 0 (gated) |
| parser, `hl_latency::parse_node_line`, 20-event 3,174-byte line | 331 (42,462 bytes): the gate panics, and the test asserts that it does |
| whole tick with a non-allocating test parser | 0 (gated) |
| whole tick with the real parser | 331 (42,462 bytes), exactly the parser's: the gate panics, and the test asserts that it does |

When the zero-allocation parser lands, the two `#[should_panic]` tests start
failing. That is the signal: delete the two attributes and both gates are
live.

## Safety

The send stage can only reach loopback. The sink address is a `LoopbackAddr`,
which is built only from an IP literal that `is_loopback()`; a host name, a
wildcard, a private or public address, or an IPv4-mapped address is refused
(`--sink-bind 8.8.8.8:443` exits 2). Every engine also checks the connected
peer address before its first write. The signing key is made up per process
from the pid and the time, never stored or printed, and controls nothing.
