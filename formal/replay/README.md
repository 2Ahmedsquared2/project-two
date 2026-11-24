# TLA+ model of the p2-node replay driver

`ReplayDriver.tla` models the replay driver from lane 3, draft PR #10, branch
`reason/p2-lane-3-p2-node-integr-5178d817`. The sync commit is
`5dc691fa1fa0d5bf81305177531c68ff5c3e7479`, which holds lane 3's fixes for
F1 and F2. The configs with `VALIDATE = FALSE` keep the earlier code at
`dc993fc1eb6d1510c2415a478093e4f2dc0452e2`, as the record of the findings.
`ReplayCli.tla` models how the CLI turns a replica file into that slice. Both
are checked with TLC on small finite instances.

This is a correctness model of our driver only. It says nothing about speed and
nothing about equivalence to native `hl-node`. A pass means the property holds
for this model, within the bounds and assumptions below.

## Run

```bash
formal/replay/run.sh          # needs Java 11+ on PATH (checked with Temurin 17.0.13)
```

`run.sh` downloads `tla2tools.jar` **v1.7.4**, the latest non-prerelease
release (v1.8.0 is a prerelease), and checks its sha256,
`936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`. It then
runs every config listed in `expected.txt` and checks that each one ends as
listed there, `pass` or `violated`. It exits non-zero if any run ends
differently. Set `TLA2TOOLS_JAR` to reuse a jar, or `TLC_LOG_DIR` to keep the
TLC logs. Each line of `expected.txt` names a config, its expected outcome and,
optionally, the module (default `ReplayDriver`). The 50 configs take about 90 s on a small cloud machine; the largest
has 2.84 M distinct states.

The repo has no `proofs/manifest.toml`, so the run command lives here.
Cache key for the team proof cache: sha256 over `formal/replay/**` plus the
covered files listed below.

## What is modelled

The driver walks a slice of `ReplayBlock`s in slice order. For each block it calls
`engine.execute_block`, then `Replay::record`. `record` does `blocks += 1`, calls
`compare(b, out)` (our per-action responses and book diffs against native's),
then either does `reproduced += 1` or calls `engine.resync(height, native_diffs)`.
At 5dc691f, `validate(blocks, snapshot)` runs first and refuses the whole slice
unless its heights are exactly `snapshot+1, snapshot+2, ...` and every block has
native diffs. The timed loop `one_pass` also refuses a raw line whose decoded
block time and round differ from the compared block's.

Books are not modelled as data. `synced` means our book equals native's book.
For each block the engine is right or wrong, as a free choice, so every
pattern of engine bugs is explored. If our book already differed from native's
before a block, that block's outputs may still equal native's (the difference
sits somewhere the block does not touch), but our book stays different.

Three switches extend this for the end-of-window question (properties E1-E3).
Each defaults to the earlier behaviour.

- `SilentDiverge`: a wrong execution may leave our book different from
  native's while the block's compared outputs still equal native's. Two
  routes in the code: the engine's emitted `book_diffs` may disagree with its
  own book (an emission bug), or the book may differ in a field `BookDiff` does
  not carry (time in force, trigger flag, client order id, timestamps).
- `MayHeal`: a later block may erase an earlier difference. For example, a
  wrong resting size is followed by a cancel of that order, which both sides
  report as the same `Remove`.
- `EndExact`: the end-of-window check sees every difference between our book
  and native's translated state.

### Action to source map (sync commit 5dc691f)

| Model action | Code at 5dc691f |
|---|---|
| `Init` (engine built from native state at `snapH`; `synced = TRUE`) | fresh engine per pass `crates/p2-node/src/timing.rs:136`, `:141`; `Replay::starting_at(snapshot)` `replay.rs:314-319`, `timing.rs:169`. Loading the snapshot into a real engine is not in the driver (only `NullEngine` and `EchoEngine` exist). |
| `Init` (`input`, `hasDiffs`) | slice built by the CLI: height `snapshot + 1 + i` per line `crates/p2-node/src/main.rs:165`; `ReplayBlock::new` `replay.rs:54-78`, block time checked against native's diffs for that height `replay.rs:60-67`; diffs `None` when absent `replay.rs:69` |
| `Init` (`pc = "refused"` unless `Valid`) | `validate` `replay.rs:150-166`: heights `replay.rs:152-160`, diffs present `replay.rs:161-163`; called by `Replay::run` `replay.rs:302` and `run_passes` `timing.rs:134` |
| `Init` (`lines`) | raw lines given to `run_passes` `timing.rs:118-124`; count check `timing.rs:128-133` |
| `LineMismatch` | `one_pass` line == compared block: time and round `timing.rs:174-183` |
| `Execute` | `for b in blocks` / `engine.execute_block` `replay.rs:304-306`; timed loop `timing.rs:172`, `:196` (executes the decoded line) |
| `RejectHeight`, `HaltNoDiffs` (`FIX = TRUE` only) | the first proposed fix; lane 3 implemented `validate` instead |
| `Compare` | `record`: `blocks += 1` `replay.rs:323`; `compare(b, out)` `replay.rs:341` -> `replay.rs:241-292`; absent native diffs compared as `&[]` `replay.rs:278` (`native_diffs()` `replay.rs:100-102`); `reproduced += 1` `replay.rs:343` |
| `Reproduced` | `None =>` arm `replay.rs:342-345`; `timing.rs:201-203` |
| `Resync` | `Some(m) =>` arm, `engine.resync(b.height, b.native_diffs())` `replay.rs:346-353`; contract `crates/p2-node/src/engine.rs:53-56` |
| `EndCheck` | not in code yet. `Replay.end_state` (`replay.rs:215-218`) is "set by the caller" and no caller sets it at 5dc691f. `to_text` prints "state identical" only when `end_state == Some(true)` and k = N (`replay.rs:361-364`). |
| `ResyncEarly`, `StopOnMismatch` | mutations only |
| k / N | `Replay.reproduced` / `Replay.blocks` `replay.rs:219-222`; `Timed.state_identical = (min reproduced, blocks)` `timing.rs:148-152`, printed `timing.rs:363-367`; like-for-like gate `main.rs:229-239` |

Covered files: `crates/p2-node/src/replay.rs`, `crates/p2-node/src/timing.rs`,
`crates/p2-node/src/engine.rs`, `crates/p2-node/src/main.rs` (slice
construction only). If any of them changes, re-read the model against the code
before moving the sync commit.

## Properties

| Id | Property (TLA+ name) | Harm if broken |
|---|---|---|
| P1 | Each block is applied exactly once, in height order: each applied block is exactly one above the previous applied block or the snapshot (`P1_ExactlyOnceInOrder`). On a well-formed slice every block is applied (`P1_AllApplied`, liveness). | Engine state built from a skipped, repeated or reordered block, so every later verdict is about the wrong state. |
| P2 | A snapshot at height H is never followed by a block at or below H (`P2_NoBlockAtOrBelowSnapshot`). | A block applied twice on top of a state that already contains it. |
| P3 | A block's verdict is fixed before the driver resyncs to native's book (`P3_VerdictBeforeResync`), it is the verdict of our own execution (`P3_VerdictIsOurs`), and it never changes afterwards (`P3_VerdictNeverChanges`). | Resync-then-compare always "matches", so k/N would report native's book as ours. |
| P4a | A block whose outputs (responses + book diffs) differ from native's is never counted as reproduced, and k = \|counted\| <= N (`P4a_OutputMismatchNeverCounted`). | Inflated k/N. |
| P4b | A block counted as reproduced also left our book equal to native's (`P4b_CountedImpliesStateIdentical`). The report calls k/N "state identical". | k/N reported as state-identical while our book has drifted. |
| E1 | A passing end-of-window check means our final book equals native's (`E1_EndPassImpliesFinalBooksAgree`). | "Final book equals native's" printed when it does not. |
| E2 | k = N and a passing end check imply our book equalled native's after **every** block of the window (`E2_AllCountedAndEndPassImpliesAgreedThroughout`). | "state identical N/N" while the book was wrong mid-window. |
| E3 | A passing end check implies every block counted in k left our book equal to native's (`E3_EndPassImpliesCountedStateIdentical`). | Partial k/N overstated despite the end check. |

## Results (tla2tools v1.7.4, TLC 2.19, MaxH = 4, MaxLen = 4)

### Code at 5dc691f (lane 3's F1/F2 fixes)

| Config | Inputs | Expected | Result |
|---|---|---|---|
| `lane3_safety` | `Replay::run`, **any** slice (gaps, repeats, swaps, at/below snapshot), diffs may be missing | P1, P2, P3, P4a, **P4b** all pass | pass (22,586 states) |
| `lane3_lines` | `one_pass`, raw lines may decode to other blocks than the compared ones | all safety properties pass | pass (1,765 states) |
| `lane3_live` | well-formed slice with diffs | every block applied | pass |

So at 5dc691f, P1, P2 and P4b hold for **any input**, missing diffs included:
`validate` refuses every slice that broke them at dc993fc before the engine
runs, and `one_pass` refuses a raw line that is not the compared block. The
model's open question 3 (executed line vs compared block) is closed as far as
block identity goes; see assumption A6. Mutations of the new checks are
below; each one fails.

### Code at dc993fc (kept as the record of F1 and F2)

| Config | Inputs | Expected | Result |
|---|---|---|---|
| `code_wellformed` | slice = snapH+1..MaxH, diffs for every block | all P1-P4b + liveness pass | pass |
| `code_any_input_holds` | any slice, diffs may be missing | P3, P4a pass | pass (2,835,968 states) |
| `code_missing_diffs_P4a` | well-formed slice, diffs may be missing | P1, P2, P4a pass | pass |
| `code_any_input_P1` / `_P2` | any slice | **F1**: P1 / P2 violated | violated |
| `code_missing_diffs_P4b` | well-formed slice, diffs may be missing | **F2**: P4b violated | violated |
| `fix_safety`, `fix_live` | the first proposed fix (`FIX = TRUE`) | pass | pass |

### End-of-window check (E1-E3)

| Config | Model | Expected | Result |
|---|---|---|---|
| `end_lane3_exact` | 5dc691f, any slice, silent divergence + healing on, exact end check | E1 passes | pass (24,092 states) |
| `end_lane3_no_silent` | 5dc691f, any slice, healing on, no silent divergence | E1, E2, E3, P4b pass | pass (22,586 states) |
| `end_lane3_no_heal` | 5dc691f, silent divergence on, nothing heals | E2, E3 pass | pass |
| `end_heal_E2` | 5dc691f, silent divergence + healing | **E2 violated (the planner's case)** | violated |
| `end_lossy_E1` | 5dc691f, end check blind to part of the book | **E1 violated** | violated |
| `end_code_exact` | dc993fc, diffs may be missing, silent + healing | E1 passes | pass |
| `end_code_no_heal` | dc993fc, diffs may be missing, silent, nothing heals | E2, E3 pass: the end check catches F2 and silent divergence | pass |
| `end_f2_heal_E3` | dc993fc, F2's `resync(h, &[])`, healing | **E3 violated**: F2 healed before the end is invisible to the end check | violated |

What this shows, within the bounds:

1. **k plus a passing end check gives "final books agree" (E1), and nothing
   more about the end.** This holds for any input and with silent divergence
   and healing on, provided the end check is exact (A5). k adds nothing to E1;
   the end check alone gives it. If the translation of native's state drops
   part of the book, the check can pass on different books (`end_lossy_E1`).
2. **It does not give "the books agreed throughout the window" (E2 fails).**
   Shortest trace (`end_heal_E2`, code at 5dc691f, every check on):
   snapshot 2, blocks 3 and 4, native diffs for both. Block 3: our engine
   leaves our book different from native's, but its responses and book diffs
   equal native's, so it is counted (k = 1). Block 4: the difference is
   overwritten (for example the order whose size we got wrong is cancelled,
   and both sides emit the same `Remove`). Outputs match (k = 2) and the books
   are equal again. End check: pass. Result: k = N = 2 with a passing end check,
   yet our book was wrong after block 3.
3. **This needs both a silent divergence and a heal.** Without healing
   (`end_lane3_no_heal`), the divergence lasts to the end and the end check
   fails. Without silent divergence (`end_lane3_no_silent`), every divergence
   shows as a mismatch at its own block, and since F2 is fixed, P4b holds on its
   own. On dc993fc the end check also catches F2, unless a later block heals it
   (`end_f2_heal_E3`).

The failure the end check still misses: a divergence that the per-block
compare cannot see (emitted diffs that disagree with the engine's own book, or
a book field `BookDiff` does not carry) and that a later block erases before
the window ends.

## CLI path: replica file to slice (`ReplayCli.tla`)

The driver results above assume `input[j]` is native's block at height
`snapshot + j` (A1). `ReplayCli.tla` checks how the CLI earns that.

How the CLI at 5dc691f builds the slice:

- Line i of the replica file gets height `snapshot + 1 + i` by position
  (`main.rs:165`), so `validate`'s height check holds by construction.
- `ReplayBlock::new(h, block, diffs.get(&h))` (`main.rs:173`) refuses the load
  (`Misaligned`, exit 1) when the line's block time in ms differs from the
  block time of native's diffs entry for height h (`replay.rs:60-67`). Native's
  diffs are keyed by native's own block number (`replay.rs:15-24`).
- `validate` refuses a height with no diffs (`replay.rs:161-163`).

The model takes any replica file of 1..4 lines over heights 1..4 (gaps,
repeats, swaps, stale lines), any snapshot 0..3, and any nondecreasing
native block times. The property is
`CLI1_AcceptedLinesAreTheirHeights`: if the load is accepted, every line is
native's block at its assumed height.

| Config | Times | Expected | Result |
|---|---|---|---|
| `cli_strict_times` | strictly increasing (A7) | pass | pass (20,400 states) |
| `cli_equal_ms_times` | two heights may share a ms time | violated | violated |
| `mut_cli_no_alignment` | mutant: no block-time check | violated | violated |
| `mut_cli_align_first_line_only` | mutant: time checked on the first line only | violated | violated |
| `probe_cli_*` | | violated | violated (accepted and refused loads both reached, including multi-line loads after a snapshot) |

So **on the CLI path, P1 and P2 hold for any replica file if and only if
native's block times strictly increase at ms resolution (A7)**. Shortest
counterexample without A7 (`cli_equal_ms_times`): genesis start, the file's
first line is block 2 (block 1 was dropped), and native's blocks 1 and 2 share a
ms block time. Line 0 is labelled height 1, its block time equals the time of
diffs entry 1, the load is accepted, and block 2 runs as block 1. A repeated
or swapped line goes through the same way.

**A7 against the data** (aggregate statistics only, nothing copied into the
model):

- `crates/p2-node/tests/data/diffs_1000.jsonl` (native diffs, 3 heights):
  times strictly increase, gaps 73 and 80 ms. This is the only native diffs
  sample in the repo.
- `bench/results/native-apply-duration-our-node-1232430001-1232432000/node_fast_block_times_first2000.jsonl`
  on lane 3's branch (d700242; sha256 in its `INPUT.txt`). It is not a diffs
  file, but it carries the same per-height `block_time`. Across 2,000
  contiguous heights, times strictly increase: no equal-ms neighbours, gaps
  36 to 154 ms, median 67 ms.
- `golden/node/node_raw_book_diffs` is a synthetic converter fixture (not
  contiguous, with one equal-ms pair). It is not native data and not
  evidence either way.

The samples support A7, but they do not prove it for every height. A7 is a
property of native's chain, and it can't be checked from these files alone.

## Mutations (each property seen failing)

Each mutation is one model step changed to mirror a plausible code bug. Every
one is in `expected.txt` as `violated` and fails with the named property.

| Mutation (`MUTANT`) | Plausible code bug | Fails |
|---|---|---|
| `validate_no_height_check` | `validate` without its height check (`replay.rs:152-160` removed) | P1 |
| `validate_ignores_snapshot` | `validate` expects `1 + index`, ignoring the snapshot | P2 |
| `validate_no_diffs_check` | `validate` without its diffs check (`replay.rs:161-163` removed); `compare` then treats absent diffs as `&[]` | P4b |
| `no_line_check` | `one_pass` without the line == compared-block check (`timing.rs:176-183` removed) | P1 |
| `end_skip_when_all_counted` | end check skipped (reported pass) when k = N | E1 |
| `end_check_before_last_block` | end check compares the book as it was before the last block | E1 |
| `no_alignment` (`ReplayCli`) | `ReplayBlock::new` without its block-time check | CLI1 |
| `align_first_line_only` (`ReplayCli`) | block time checked on the first line only | CLI1 |
| `guard_gt` | (first proposed fix) height guard written as `h > last`, so gaps pass | P1 |
| `guard_ge` | (first proposed fix) off by one, `h >= last`, so duplicates pass | P1 |
| `stop_on_mismatch` | `return` on the first mismatch instead of resync | P1 liveness |
| `cursor_ignores_snapshot` | height cursor starts at 0, not the snapshot height | P2 |
| `resync_before_compare` | resync moved above `compare` | P3 (`VerdictBeforeResync`, and `VerdictIsOurs`) |
| `recompare_after_resync` | block compared again after resync | P3 (`VerdictNeverChanges`) |
| `count_every_block` | `reproduced += 1` outside the `None` arm | P4a |
| `resync_without_diffs` | the first fix's stop on missing diffs removed | P4b |

E2 and E3 are already seen failing on unmutated models (`end_heal_E2`,
`end_f2_heal_E3`).

Vacuity probes (`probe_*`, all violated as expected) show what the models
reach. The 5dc691f model reaches a refusal, a mismatch with a resync, and a
snapshot followed by an applied block. The dc993fc model reaches a resync, a
finished run with both a mismatch and a reproduced block, and n > k, and its end
check can both fail and pass after a resync.

## Bounds and assumptions

- MaxH = 4 heights, MaxLen = 4 blocks per slice, snapshot height 0..3. In the
  any-slice configs, every slice of length 1..4 over heights 1..4 is checked.
  Run once by hand at larger bounds (not in `run.sh`), all passing:
  - at MaxH = MaxLen = 5: `lane3_safety` (627,386 states), `lane3_lines`
    (16,777), `end_lane3_no_silent` (627,386) and `end_lane3_exact` (634,371);
  - earlier, at dc993fc: `code_wellformed` and `fix_live` at 7/7,
    `code_missing_diffs_P4a` at 6/6, and `fix_safety` at 5/5.
- A1. Heights are block identities: a `ReplayBlock`'s height matches its
  content. On the CLI path this is checked, not assumed: `ReplayCli.tla` shows
  that it follows from A7.
- A7. Native's block times strictly increase with height at ms resolution, so
  no two heights share a ms block time. See "A7 against the data" above.
- `ReplayCli`: heights 1..4, replica files of 1..4 lines, snapshot 0..3,
  block times in 1..6 ms, all nondecreasing time functions.
- A2. `engine.resync(h, d)`, given native's complete diffs for block h and a book
  that equalled native's before h, leaves our book equal to native's after h.
  This is the engine's contract (`engine.rs:53-56`); the model assumes it.
- A3. `engine.resync(h, &[])` either leaves our book as `execute_block` made it,
  or loses this block's changes. It never restores native's changes. Both
  choices are explored. At 5dc691f this path is reachable only if `validate` is
  bypassed.
- A4. With `SilentDiverge = FALSE`: a block's outputs on an equal pre-block
  book equal native's exactly when the engine executed it like native (`ok`).
  With `SilentDiverge = TRUE`, a wrong execution may still produce native's
  outputs. On a different pre-block book they may or may not match (`lucky`).
- A5. The end check (proposed; not in code at 5dc691f) compares our final
  book with native's state at the last height, translated into our book's
  shape, and fails exactly when they differ (`EndExact = TRUE`).
  `end_lossy_E1` drops this assumption.
- A6. `one_pass` checks the decoded line's block time and round against the
  compared block (`timing.rs:176`), which identifies the block. It does not
  compare the actions. In the CLI both come from the same line (`main.rs:166`),
  so they are equal by construction there.
- Liveness uses weak fairness on every driver step.
- Lane 3's later head d700242 only refactors how `UserAction` is built
  (`UserAction::from_signed`) and shifts line numbers. It does not change the
  modelled behaviour, and the sync commit stays 5dc691f.

## Not covered

- The engine itself (lane 1's `hl-exec`), `compare`'s field-by-field logic, JSON
  equality, and `hl-block` decoding.
- Native diffs files with a repeated `block_number` (`load_native_diffs` keeps
  the last one, `replay.rs:21`), and a replica file from a different chain or
  fork.
- Loading the snapshot into the engine, and the translation of native's state
  for the end check (A5): neither exists in code yet.
- Which book fields `BookDiff` leaves out, and whether lane 1's engine can emit
  diffs that disagree with its own book: these decide whether `SilentDiverge`
  can happen in practice.
- Warm-up gating, timing, CSV output, and multiple passes (each pass is the same
  loop on a fresh engine).
- Anything about speed, and any claim of equivalence with native.
