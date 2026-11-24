# TLA+ model of the p2-node replay driver

`ReplayDriver.tla` models the replay driver from lane 3, draft PR #10, branch
`reason/p2-lane-3-p2-node-integr-5178d817`. The sync commit is
`9219ae731427908a426042ff02b40cc441c06018` (`VALIDATE = TRUE`, `V9219 = TRUE`).
It keeps lane 3's F1/F2 fixes from `5dc691fa1fa0d5bf81305177531c68ff5c3e7479`
and adds a decoded-height check, a whole-block line check in `one_pass` and
one verdict label. 5dc691f's behaviour is kept as recorded mutants
(`line_check_time_round_only`, `label_state_identical`, and `HeightField =
"never"` in `ReplayCli`). The configs with `VALIDATE = FALSE` keep the earlier
code at `dc993fc1eb6d1510c2415a478093e4f2dc0452e2`, as the record of the
findings.
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
optionally, the module (default `ReplayDriver`). The 61 configs take about
3.5 minutes on a shared cloud machine; the largest has 2.84 M distinct states.

The repo has no `proofs/manifest.toml`, so the run command lives here.
Cache key for the team proof cache: sha256 over `formal/replay/**` plus the
covered files listed below.

## What is modelled

The driver walks a slice of `ReplayBlock`s in slice order. For each block it calls
`engine.execute_block`, then `Replay::record`. `record` does `blocks += 1`, calls
`compare(b, out)` (our per-action responses and book diffs against native's),
then either does `reproduced += 1` or calls `engine.resync(height, native_diffs)`.
At 9219ae7, `validate(blocks, snapshot)` runs first and refuses the whole slice
unless its heights are exactly `snapshot+1, snapshot+2, ...`, every block that
carries its own height carries that one, and every block has native diffs.
The timed loop `one_pass` decodes each raw line, executes it, stops the
timers, and then refuses the pass (`Err`, which drops the whole timed run)
unless the decoded block equals the compared block in full. 5dc691f compared
only block time and round there.

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

### Action to source map (sync commit 9219ae7)

| Model action | Code at 9219ae7 |
|---|---|
| `Init` (engine built from native state at `snapH`; `synced = TRUE`) | fresh engine per pass `crates/p2-node/src/timing.rs:138`, `:143`; `Replay::starting_at(snapshot)` `replay.rs:342`, `timing.rs:171`. Loading the snapshot into a real engine is not in the driver (only `NullEngine` and `EchoEngine` exist). |
| `Init` (`input`, `hasDiffs`) | slice built by the CLI: height `snapshot + 1 + i` per line `crates/p2-node/src/main.rs:168`; `ReplayBlock::new` `replay.rs:54-78`, block time checked against native's diffs for that height `replay.rs:60-67`; diffs `None` when absent `replay.rs:69` |
| `Init` (`pc = "refused"` unless `Valid`) | `validate` `replay.rs:149-173`: heights `replay.rs:151-159`, diffs present `replay.rs:168-170` (decoded height `replay.rs:160-167` is modelled in `ReplayCli.tla`); called by `Replay::run` `replay.rs:330` and `run_passes` `timing.rs:136` |
| `Init` (`lines`, `altered`) | raw lines given to `run_passes` `timing.rs:120-126`; count check `timing.rs:130-135` |
| `LineMismatch` | `one_pass`: decode `timing.rs:176-177`, execute `timing.rs:178-183`, then `if &block != rb.block() { return Err(Mismatch) }` `timing.rs:188-195`; the `Err` leaves before `record` and `?` drops the run `timing.rs:138`, `:142-148` |
| `Execute` | `for b in blocks` / `engine.execute_block` `replay.rs:332-334`; timed loop `timing.rs:174`, `:180` (executes the decoded line) |
| `RejectHeight`, `HaltNoDiffs` (`FIX = TRUE` only) | the first proposed fix; lane 3 implemented `validate` instead |
| `Compare` | `record`: `blocks += 1` `replay.rs:351`; `compare(b, out)` `replay.rs:372` -> `replay.rs:269-320`; absent native diffs compared as `&[]` `replay.rs:306` (`native_diffs()` `replay.rs:98-102`); `reproduced += 1` `replay.rs:374` |
| `Reproduced` | `None =>` arm `replay.rs:373-376`; `timing.rs:198-205` |
| `Resync` | `Some(m) =>` arm, `engine.resync(b.height, b.native_diffs())` `replay.rs:377-385`; contract `crates/p2-node/src/engine.rs:67-71` |
| `EndCheck` (`endState`) | not in code yet. `Replay.end_state` (`replay.rs:243-246`) is "set by the caller" and no caller sets it at 9219ae7. |
| `EndCheck` (`claimsSI`, the label) | `verdict_label` `replay.rs:175-194`, used by `to_text` `replay.rs:390-393`, the timing summary `timing.rs:360-363` and the CLI `main.rs:244`. It never prints "state identical". |
| `ResyncEarly`, `StopOnMismatch` | mutations only |
| k / N | `Replay.reproduced` / `Replay.blocks` `replay.rs:248-250`; `Timed.outputs_matched = (min reproduced, blocks)` `timing.rs:150-153`; like-for-like gate `main.rs:232-239` (needs `end_state == Some(true)`, which no caller sets, so every row is "not like-for-like") |

Covered files: `crates/p2-node/src/replay.rs`, `crates/p2-node/src/timing.rs`,
`crates/p2-node/src/engine.rs`, `crates/p2-node/src/main.rs` (slice
construction and labels only). If any of them changes, re-read the model
against the code before moving the sync commit.

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
| P5 | The timed pass executes exactly the block it compares, in identity and content (`P5_ExecutedIsCompared`). | Timing and verdicts of one block reported against another block's native outputs. |
| P6 | A pass that ends normally compared every block of the slice (`P6_FinishedPassCoversSlice`). | A short pass reported as the whole window. |
| L1 | If a report says "state identical", every counted block left our book equal to native's (`L1_StateIdenticalClaimHolds`). | The E2 trace printed as "state identical N/N". |

## Results (tla2tools v1.7.4, TLC 2.19, MaxH = 4, MaxLen = 4)

### Code at 9219ae7 (sync commit)

| Config | Inputs | Expected | Result |
|---|---|---|---|
| `lane3_safety` | `Replay::run`, **any** slice (gaps, repeats, swaps, at/below snapshot), diffs may be missing | P1, P2, P3, P4a, **P4b**, P6 all pass | pass (22,586 states) |
| `lane3_lines` | `one_pass`, raw lines may decode to other blocks, or to the same block (time, round) with other content | all safety properties, **P5**, P6 pass | pass (14,785 states) |
| `lane3_live` | well-formed slice with diffs | every block applied | pass |
| `label_L1` | silent divergence + healing (the E2 trace) | L1 passes: no report says "state identical" | pass (2,557 states) |

So at 9219ae7, P1, P2 and P4b hold for **any input**, missing diffs included:
`validate` refuses every slice that broke them at dc993fc before the engine
runs. `one_pass` executes exactly the block it compares (P5) without
assumption A6: a line with the right time and round but other content is now
refused (`mut_line_check_time_round_only` is 5dc691f's check and fails P5).
The line is executed before the check, but the `Err` drops the whole timed
run, so nothing from it is compared, counted or reported. The E2 trace below
still exists, but at 9219ae7 no report turns it into a false "state
identical" claim (L1; `mut_label_state_identical` puts 5dc691f's labels back
and fails L1). Mutations of the new checks are below; each one fails.

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
| `end_lane3_exact` | 9219ae7, any slice, silent divergence + healing on, exact end check | E1 passes | pass (24,092 states) |
| `end_lane3_no_silent` | 9219ae7, any slice, healing on, no silent divergence | E1, E2, E3, P4b pass | pass (22,586 states) |
| `end_lane3_no_heal` | 9219ae7, silent divergence on, nothing heals | E2, E3 pass | pass |
| `end_heal_E2` | 9219ae7, silent divergence + healing | **E2 violated (the planner's case)** | violated |
| `end_lossy_E1` | 9219ae7, end check blind to part of the book | **E1 violated** | violated |
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
   Shortest trace (`end_heal_E2`, code at 9219ae7, every check on):
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

How the CLI at 9219ae7 builds the slice:

- Line i of the replica file gets height `snapshot + 1 + i` by position
  (`main.rs:168`), so `validate`'s height check holds by construction.
- `ReplayBlock::new(h, block, diffs.get(&h))` (`main.rs:176`) refuses the load
  (`Misaligned`, exit 1) when the line's block time in ms differs from the
  block time of native's diffs entry for height h (`replay.rs:60-67`). Native's
  diffs are keyed by native's own block number (`replay.rs:15-24`).
- New at 9219ae7: `validate` refuses a line that carries its own top-level
  `height` (`Block.height`) when it is not h (`ReplayError::DecodedHeight`,
  `replay.rs:160-167`). A line without the field is not checked this way.
- `validate` refuses a height with no diffs (`replay.rs:168-170`).

The model takes any replica file of 1..4 lines over heights 1..4 (gaps,
repeats, swaps, stale lines), any snapshot 0..3, any nondecreasing native
block times, and lines that carry the height field always, never, or per
line (`HeightField`). The properties are `CLI1_AcceptedLinesAreTheirHeights`
(if the load is accepted, every line is native's block at its assumed
height) and `CLI2_CorrectLoadAccepted` (a file that is exactly native's
blocks snapshot+1, +2, ... is accepted).

| Config | Times | Height field | Expected | Result |
|---|---|---|---|---|
| `cli_strict_times` | strictly increasing (A7) | per line | CLI1, CLI2 pass | pass (280,800 states) |
| `cli_height_always_equal_ms` | may tie at ms | every line | CLI1, CLI2 pass | pass (171,360 states) |
| `cli_height_mixed_equal_ms` | may tie at ms | per line | CLI1 violated | violated |
| `cli_equal_ms_times` | may tie at ms | no line (= 5dc691f) | CLI1 violated | violated |
| `mut_cli_no_alignment` | strictly increasing | per line | mutant: no block-time check, CLI1 violated | violated |
| `mut_cli_align_first_line_only` | strictly increasing | per line | mutant: time checked on the first line only, CLI1 violated | violated |
| `mut_cli_no_decoded_height_check` | may tie at ms | every line | mutant: no `DecodedHeight` check, CLI1 violated | violated |
| `mut_cli_decoded_height_ignores_snapshot` | strictly increasing | every line | mutant: decoded height compared with `1 + i`, CLI2 violated | violated |
| `probe_cli_*` | | | violated | violated (accepted and refused loads both reached, including multi-line loads after a snapshot, and a load refused by the decoded-height check alone) |

So on the CLI path at 9219ae7, P1 and P2 hold for any replica file when
**either** every line carries its height field (A8, no condition on block
times) **or** native's block times strictly increase at ms resolution (A7).
If some line lacks the field and two neighbouring heights share a ms block
time, a misplaced line still passes. Shortest counterexample
(`cli_height_mixed_equal_ms`, same as `cli_equal_ms_times`): genesis start, a
one-line file holding block 2 without a height field (block 1 was dropped),
and native's blocks 1 and 2 share a ms block time. The line is labelled
height 1, its block time equals the time of diffs entry 1, nothing else is
checked, the load is accepted, and block 2 runs as block 1. A repeated or
swapped line goes through the same way.

**The height field against the data** (counts only): no replica block file in
the repo carries a top-level `height`. That includes
`crates/hl-block/tests/data/three_blocks.jsonl` and
`crates/p2-node/tests/data/three_blocks.jsonl` (3 lines each, 0 with height),
and `bench/results/b2-warm-vs-cold-cloud-869b282/blocks.jsonl` and
`pw-blocks.jsonl` (200 lines each, 0 with height). The `hl-block` docs say our
node's own capture (blocks 1232430001..=1232432000) has the field, but that
capture is not in the repo. On every replica file in the repo, the CLI path
therefore still rests on A7.

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
| `validate_no_height_check` | `validate` without its height check (`replay.rs:151-159` removed) | P1 |
| `validate_ignores_snapshot` | `validate` expects `1 + index`, ignoring the snapshot | P2 |
| `validate_no_diffs_check` | `validate` without its diffs check (`replay.rs:168-170` removed); `compare` then treats absent diffs as `&[]` | P4b |
| `no_line_check` | `one_pass` without the line == compared-block check (`timing.rs:188-195` removed) | P1 |
| `line_check_time_round_only` | 5dc691f's check: time and round only, so a line with other actions passes | P5 |
| `line_mismatch_continue` | on a line mismatch, `continue` (skip the block) instead of `return Err` | P1 |
| `line_mismatch_break` | on a line mismatch, `break` and report the pass as complete | P6 |
| `label_state_identical` | 5dc691f's labels: "state identical k/N" printed again | L1 |
| `no_decoded_height_check` (`ReplayCli`) | `validate` without its `DecodedHeight` check (`replay.rs:160-167` removed) | CLI1 |
| `decoded_height_ignores_snapshot` (`ReplayCli`) | decoded height compared with `1 + i` instead of `snapshot + 1 + i` | CLI2 |
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
reach. The 9219ae7 model reaches a refusal, a mismatch with a resync, a
snapshot followed by an applied block, and a raw line that is the compared
block by time and round but differs in content. The dc993fc model reaches a resync, a
finished run with both a mismatch and a reproduced block, and n > k, and its end
check can both fail and pass after a resync.

## Bounds and assumptions

- MaxH = 4 heights, MaxLen = 4 blocks per slice, snapshot height 0..3. In the
  any-slice configs, every slice of length 1..4 over heights 1..4 is checked.
  Run once by hand at larger bounds (not in `run.sh`), all passing:
  - at MaxH = MaxLen = 5, code at 9219ae7: `lane3_safety` (627,386 states),
    `lane3_lines` (319,387), `label_L1` (23,925) and
    `cli_height_always_equal_ms` (9,020,550);
  - at MaxH = MaxLen = 5, code at 5dc691f: `end_lane3_no_silent` (627,386)
    and `end_lane3_exact` (634,371);
  - earlier, at dc993fc: `code_wellformed` and `fix_live` at 7/7,
    `code_missing_diffs_P4a` at 6/6, and `fix_safety` at 5/5.
- A1. Heights are block identities: a `ReplayBlock`'s height matches its
  content. On the CLI path this is checked, not assumed: `ReplayCli.tla` shows
  that it follows from A8 for lines with a height field, and from A7
  otherwise.
- A8. A line's top-level `height` field, when present, is that block's own
  height as native numbers it. Our node writes the field; the model does not
  check where it comes from.
- A7. Native's block times strictly increase with height at ms resolution, so
  no two heights share a ms block time. See "A7 against the data" above.
- `ReplayCli`: heights 1..4, replica files of 1..4 lines, snapshot 0..3,
  block times in 1..6 ms, all nondecreasing time functions, every pattern of
  lines with and without the height field.
- A2. `engine.resync(h, d)`, given native's complete diffs for block h and a book
  that equalled native's before h, leaves our book equal to native's after h.
  This is the engine's contract (`engine.rs:67-71`); the model assumes it.
- A3. `engine.resync(h, &[])` either leaves our book as `execute_block` made it,
  or loses this block's changes. It never restores native's changes. Both
  choices are explored. Since 5dc691f this path is reachable only if `validate` is
  bypassed.
- A4. With `SilentDiverge = FALSE`: a block's outputs on an equal pre-block
  book equal native's exactly when the engine executed it like native (`ok`).
  With `SilentDiverge = TRUE`, a wrong execution may still produce native's
  outputs. On a different pre-block book they may or may not match (`lucky`).
- A5. The end check (proposed; not in code at 9219ae7) compares our final
  book with native's state at the last height, translated into our book's
  shape, and fails exactly when they differ (`EndExact = TRUE`).
  `end_lossy_E1` drops this assumption.
- A6 (5dc691f only, no longer needed). At 5dc691f `one_pass` checked only
  block time and round, so the model assumed those identify the whole block.
  At 9219ae7 `one_pass` compares the whole decoded block (`timing.rs:188`),
  and `lane3_lines` drops A6: a line may match by time and round and differ
  in content.
- Liveness uses weak fairness on every driver step.
- Lane 3's head d700242 only refactored how `UserAction` is built. 9219ae7
  is the sync commit now. Its other changes are not modelled: `record` counts
  executed actions only up to the block's action count (`replay.rs:362-370`,
  feeds `actions_executed`, not k or N), and the CLI's like-for-like gate
  now also needs `end_state == Some(true)` (`main.rs:232-239`).

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
- In `ReplayCli`, replica lines from heights that have no native diffs (the
  CLI refuses such a load anyway, `replay.rs:168-170`), and whether our
  node's `height` field is right (A8).
- Anything about speed, and any claim of equivalence with native.
