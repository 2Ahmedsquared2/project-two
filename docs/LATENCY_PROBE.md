# Same-block latency: our node vs the public WebSocket

Asked by the Latency Reason: the node only matters for HFT if it shows us a
block earlier than `wss://api.hyperliquid.xyz/ws`. This measures that, per block.

## Method

One probe process on the node host, one clock (`CLOCK_REALTIME`, disciplined by
chrony on the PHC; the difference of two stamps on one host does not depend on
sync with anyone else).

- **Node arm:** tail the newest hourly file under `~/hl/data/node_raw_book_diffs`
  (or `node_order_statuses`), written with `--stream-with-block-info
  --disable-output-file-buffering`. Stamp each line when `read` returns it
  (inotify `IN_MODIFY` wake-up, then read). First line seen for a block = the
  node arrival. Parsed by `hl_latency::parse_node_line`.
- **WS arm:** one connection, subscribed to `l2Book` and `trades` for the top
  coins (BTC, ETH, SOL, HYPE). Stamp each frame when the TLS read completes, before
  JSON parsing. Earliest frame carrying a block's time = the WS arrival. Parsed by
  `hl_latency::parse_ws_message`. **Drop the first frame after every
  (re)subscribe:** it is a snapshot (the first `trades` frame on 2026-11-19
  carried trades ~15 s old).
- **Stamp point (both arms):** `recv_ns` is taken immediately after the
  socket/file `read` returns, before any parsing or JSON decode. `hl-latency`
  never stamps; it only receives `recv_ns` from the capture side, so the
  capture binary owns this rule and must test it.
- **Join:** block time in ms (`hl_latency::pair`); the node line also gives the
  height. Blocks seen by one side only are counted, never dropped silently. A
  block time the node reports with two heights is *ambiguous*: counted and
  excluded from the distribution (the WS has no height to tell them apart).
- **Report:** `hl-latency observations.jsonl` prints the node lead
  (`ws_ns - node_ns`, positive = node earlier): `matched` (the sample count
  behind every percentile), `node_only`, `ws_only`, `ambiguous`,
  `key_collisions`, node-first count, min/p50/p90/p99/p99.9/max in ns, and
  `per_hour` buckets (sample count, p50, p99, max per UTC hour of block time)
  to catch drift. One-side-only counts are results, not noise. Treat p99.9 as
  weak under about 10,000 matched blocks (about 12 minutes at ~70 ms blocks).

Observation line format (one per arrival):

```json
{"source":"node","block_time_ms":1753606210051,"block_number":676607012,"recv_ns":1753606210402113000}
{"source":"public_ws","block_time_ms":1753606210051,"recv_ns":1753606210467000000}
```

## Host prep (before every run; done with/through the AWS Reason)

- ENA Express off on the interface.
- chrony on the Amazon Time Sync source (PHC where present); record
  `chronyc tracking` offset before and after each run, and at the end of
  the 24 h run (the clock can drift over a day).
- Verify PHC on the instance: `ls /dev/ptp*` and `ethtool -T <iface>`.
- Probe pinned to its own core (`taskset`), away from the node's cores.
- CPU governor `performance` if the instance allows it; record either way.
- `ethtool -S` allowance counters (`*_allowance_exceeded`) before/after each run;
  any increase invalidates the run.

## Run plan

1. 1 h smoke run, then 24 h, same host, both arms simultaneously; read the
   `per_hour` buckets for drift.
2. Repeat with the WS arm on a second region-local box only if the first host's
   CPU load could skew the WS arm (check probe CPU < 5 %).
4. Second experiment (later): our own node's output vs `hl-node`'s, same method.

## Caveats

- This measures "time to a usable byte in our process", which is what a
  strategy sees; it includes the node's file write and our read.
- `l2Book` frames may be sent only when the book changed; quiet coins are not
  evidence. Using several active coins and `trades` covers most blocks.
- Public WS behaviour (batching, throttling) is not documented in detail;
  results are a measurement of today's service, not a constant.

## Status

Done: parser, join, nearest-rank statistics, ambiguous-key exclusion,
per-hour buckets, report CLI with contract tests, proptests and a cargo-fuzz
target (`fuzz/`, `cargo +nightly fuzz run hl_latency_parsers`), tests
including real captured WS frames.

Done (capture, `crates/hl-capture`): node arm. `hl-capture node <dir> --out
obs.jsonl [--detect busy-poll|poll|inotify] [--poll-us N] [--duration-s N]
[--max-line-bytes N] [--ring-records N] [--rescan-ms N]` follows the newest hourly file
(numeric `{date}/{hour}` order, so hour 10 is newer than 9), reads a file that
exists at start from its end and every later file from byte 0, drains the old
file before switching at rotation, and stamps `CLOCK_REALTIME` once per `read`
that returns data, before parsing; a line split across reads carries the stamp
of the read that delivered its newline. Bad lines are counted, not fatal. Lines longer than `--max-line-bytes`
(default 1 MiB) are dropped and counted (`oversized_lines`), so a stream
without newlines holds at most that much. `--duration-s` is a hard deadline for reading:
a wait is cut short, and `{"reader_stopped":true}` goes to stderr when reading
ends. The writer thread then writes every record the ring still holds, so the
process exits after the deadline by however long that backlog takes (measured
0.5 to 0.95 s for 350k to 600k queued records, debug build, loaded shared
machine); nothing already read is discarded.

`SIGTERM` or `SIGINT` (Ctrl-C) ends a run the same way as the deadline: the
reader stops, prints a `signal` line and `{"reader_stopped":true}`, the writer
writes what the ring holds, the counters are printed with
`"stop_signal":15` (or `2`; `null` for a run no signal stopped), and the exit
status is 0. Such a run is as valid as a `--duration-s` run and is judged by
`ring_dropped` like one; bytes the node wrote that the reader had not read yet
are not captured. `busy-poll` and the default `poll` notice the signal at
once; a long `--poll-us` and `inotify` notice it within 100 ms (every wait is
capped at that, about ten empty reads a second while idle). **A second signal
ends the process at once** with status 128 + the signal number (143, 130), no
counters and possibly a cut-off last record: that run is invalid. Use it only
when the writer cannot finish (a disk that stopped accepting writes).
`SIGKILL` and `SIGHUP` are not handled: they lose the ring and the counters.
A signal counts as the first one whenever it arrives. If the reader has
already stopped (the `--duration-s` deadline passed and the writer is still
writing the ring), the first signal changes nothing: the writer carries on,
no `signal` line is printed and `stop_signal` stays `null`. A second signal
then ends the process as above. (From reading the code; no test covers a
signal during the final write.)
After its first poll it writes `{"ready":true}` to stderr (after the `detect`
line); a harness that creates or renames in a file it expects read must wait
for that line first, or a slow start can make the file count as history (read
from its end).

How it waits for the node to write (`--detect`; the live mode and the reason
for any fallback are printed on stderr as a `detect` line):

- `busy-poll` (default): spins on `read`, one core at 100%; give it its own
  core. Falls back to `poll` when only one CPU is usable.
- `poll`: sleeps `--poll-us` (default 100) between reads.
- `inotify`: sleeps until the kernel reports a write. Linux only; off Linux,
  or when inotify cannot be set up, it falls back to `poll`. The watch on a
  file is added after that file's first read, so the wait that follows does
  not sleep and the file is read once more: a line written before the watch
  existed is stamped then, not at the next write.

Reader and writer are separate threads. The reader's tick is read, split,
stamp, header scan (`hl_latency::scan_node_header`) and a push of a 32-byte
record into a bounded SPSC ring (`rtrb`, `--ring-records`, default 65536); it
does not allocate (gated by `tests/alloc.rs` with `hl-alloc-gate`), serialise
or touch the disk. The writer thread serialises and flushes. **Full ring: the
newest record is dropped and counted (`ring_dropped`); the reader never
blocks.** A blocked reader would stamp every later line late, which looks like
a slow node; a counted drop is only a missing sample. **A run with
`ring_dropped` above 0 is invalid: discard it.**

The strict parser (`parse_node_line`) runs on the reader only for a line the
header scan refuses (`scan_fallbacks`, expected 0 on hl-node output). Lines the
scan accepts are not strict-validated by this tool: `height_breaks` counts
block numbers that do not follow on, and the offline strict pass over the same
hourly files (`hl-strict-pass`, below) is the check for writer-torn lines.

### Offline strict pass (`crates/hl-strict-pass`)

Run after a capture, never beside the reader:
`hl-strict-pass [--max-failures N] [--max-line-bytes N]
[--allow-unterminated-tail] [--fail-on-height-gap] <path>...`, where a path is
an hourly file or a directory whose `{date}/{hour}` files are read oldest
first. A record is a newline-terminated line, and it passes when it is valid
UTF-8, `hl_latency::parse_node_line` accepts it (the whole line is one JSON
object with a timestamp `block_time` and a u64 `block_number`), its top level
has `local_time` (string) and `events` (array) with none of the four keys
repeated, and the header scan, where it answers, agrees with the strict parse.
What the events hold is not checked.

stdout is JSON lines: a `failure` line per failed record (`path`, `line`,
`offset`, `len`, `reason`, `scan_accepted`, and for a scan-accepted line the
`block_number` and `block_time_ms` hl-capture recorded for it, so that
observation can be dropped from the pairing); a `file` line per file with its
counters and first/last `block_number` and `block_time_ms`; one `total` line.
Counters: `records`, `failed`, `scan_accepted_failed` (failed lines the reader
stamped), `unterminated_tail`, `height_breaks` split into `height_repeats`,
`height_gaps` and `height_backwards`, `block_time_backwards`,
`duplicate_block_numbers` (a height seen earlier in the pass, other than a
repeat of the line before: an overlapping or rewritten file). At most
`--max-failures` (default 100) failures are listed; the counts are never
capped and `failures_omitted` says how many were left out. A line over
`--max-line-bytes` (default 64 MiB) fails as `oversized` without being parsed.
A last line with no newline fails as `unterminated`, unless
`--allow-unterminated-tail`, which applies to the last file only (the hour
still being written).

Heights are compared with the last record that passed, across files; a failed
record is not a reference. hl-capture's own `height_breaks` also counts from
torn lines its scan accepted, so the two numbers can differ on a damaged file.
Height breaks do not fail the run: with `--stream-with-block-info` a block can
span several lines. `--fail-on-height-gap` makes gaps and backward steps (not
repeats) fail it.

Exit codes: 0 every record passed, 1 a record failed (or a gap, when asked),
2 usage, 3 unreadable path or no hourly file found.

Feed its stdout to the report: `hl-latency observations.jsonl --strict-pass
strict-pass.jsonl` leaves every block listed as a scan-accepted failure out of
the pairing. An observation carries no file offset, so a torn line's stamp
cannot be told from a good line of the same block: the whole block goes (every
node observation with that `block_number` and `block_time_ms`, and the
public-WebSocket observations with that block time), and it counts as neither
matched, node-only nor ws-only. The summary gains `strict_pass_applied`,
`strict_dropped_observations`, `strict_dropped_blocks`,
`strict_dropped_ws_arrivals`, `strict_failures_listed`,
`strict_failures_unmatched` (listed blocks the capture never observed),
`strict_failures_omitted` and `strict_drop_incomplete`. When the strict pass
capped its list (`failures_omitted` above 0) the summary is still printed but
`hl-latency` exits 1: torn stamps may remain, so rerun `hl-strict-pass` with a
larger `--max-failures`. Strict-pass output that is malformed or has no
`total` line (cut short) exits 2 with no summary. Without `--strict-pass`,
`strict_pass_applied` is false and nothing was dropped.

A newer hourly file is looked for every `--rescan-ms` (default 1000) and, in
`inotify` mode, when a directory changes; never per poll, and only once the
current file is read to its end. When the look finds a newer file, the old one
is read to its end once more before the switch, so a line the node appended to
it while the look ran is kept (those are ordinary lines, not post-rotation
ones). What the node appends to the old file after that is not read: the node
is taken to finish a file before it starts the next (not verified against real
`hl-node` output). The look is a directory walk on the reader
thread (13 us with one date directory, 262 us with thirty, shared machine):
keep the interval coarse and **prune old date directories**. What the
switching poll reads from a new file may be stamped up to `--rescan-ms` late:
it is reported as a `rotation` line on stderr (`late_lines`,
`first_block_number`, `last_block_number`) and counted in
`post_rotation_lines`; leave those block numbers out of percentiles. Moving
the walk to the writer thread is the follow-up.

Counters at exit (stderr, one JSON object): `lines`, `written`, `ring_dropped`,
`bad_lines`, `oversized_lines`, `scan_fallbacks`, `height_breaks`, `rotations`,
`post_rotation_lines`, `rescans`, `reads`, `stop_signal`.
`WsFilter` drops the first `l2Book`/`trades` frame per coin after each
(re)connect and counts malformed frames, including `l2Book`/`trades` frames
whose coin is missing, null, empty, not a string or (for `trades`) not the
same in every trade. A second cargo-fuzz target, `hl_capture_inputs`, feeds
arbitrary chunks through the line splitter and filter. Tested against fake node files
(append, partial line, rotation, late-appearing file) and synthetic and
captured WS frames.

Next: the WS socket client feeding `WsFilter` (stamp at TLS read), then
the 1 h run on the host.
