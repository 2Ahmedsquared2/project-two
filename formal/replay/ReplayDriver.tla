---------------------------- MODULE ReplayDriver ----------------------------
(***************************************************************************)
(* Model of the p2-node replay driver: Replay::run / Replay::record and     *)
(* the timed loop one_pass (crates/p2-node/src/{replay,timing}.rs, lane 3,  *)
(* draft PR #10). Sync commit 9219ae7 (VALIDATE = TRUE, V9219 = TRUE).     *)
(* V9219 = FALSE keeps 5dc691f's one_pass line check (time and round only)  *)
(* and its "state identical" label; VALIDATE = FALSE keeps dc993fc and its  *)
(* findings F1, F2. README.md maps every action below to file:line, lists  *)
(* bounds and assumptions, and what is NOT covered.                         *)
(*                                                                         *)
(* The driver walks a slice of blocks in slice order. Per block it calls    *)
(* engine.execute_block, then record(): blocks += 1, compare(b, out) (our   *)
(* per-action responses and book diffs vs native's), then either            *)
(* reproduced += 1 or engine.resync(height, native_diffs).                  *)
(*                                                                         *)
(* Abstraction. Books are not data. `synced` = our book equals native's     *)
(* book at the current point. Per block the engine is right or wrong        *)
(* (free choice `ok`), so every pattern of engine bugs is explored. If our  *)
(* book was already different from native's before a block, that block's   *)
(* outputs may still equal native's (the difference sits where the block    *)
(* does not touch: free choice `lucky`), but our book stays different.      *)
(*                                                                         *)
(* FIX = FALSE, VALIDATE = FALSE: code at dc993fc. VALIDATE = TRUE: code   *)
(* at 5dc691f. Silent divergence, healing and the end-of-window check are   *)
(* the extension for the k/N headline gate (E1-E3).                         *)
(* FIX = TRUE models the fix first proposed to lane 3: refuse a block whose *)
(* height is not last+1, stop when native's diffs are missing. MUTANT       *)
(* selects one                                                              *)
(* deliberate bug used to show each property can fail ("none" = no bug).    *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    MaxH,              \* block heights are 1..MaxH
    MaxLen,            \* longest input slice (>= MaxH)
    AnyInput,          \* TRUE: slice is any sequence of heights (duplicates, gaps,
                       \*   reordering, blocks at or below the snapshot);
                       \* FALSE: slice is exactly snapH+1, snapH+2, ..., MaxH
    DiffsMayBeMissing, \* TRUE: native's book diffs may be absent for any block
    FIX,               \* FALSE: code at dc993fc; TRUE: proposed fix
    MUTANT,            \* "none" or a mutation name from the README
    SilentDiverge,     \* TRUE: a wrong execution may leave our book different
                       \*   from native's while the block's compared outputs
                       \*   (responses + book diffs) still equal native's
    MayHeal,           \* TRUE: a later block may erase an earlier difference,
                       \*   so our book equals native's again
    VALIDATE,          \* TRUE: lane 3's fix at 5dc691f: validate() refuses the whole
                       \*   slice up front unless heights are snapH+1, snapH+2, ...
                       \*   and every block has native diffs; one_pass refuses a raw
                       \*   line that is not the compared block
    LinesMayDiffer,    \* TRUE: timed pass (one_pass) whose raw line j may decode to a
                       \*   different block than the compared block j
    EndExact,          \* TRUE: the end-of-window check sees every difference
                       \*   between our book and native's translated state
    V9219              \* TRUE: code at 9219ae7 (needs VALIDATE): one_pass compares
                       \*   the whole decoded block with the compared one, and no
                       \*   report prints "state identical". FALSE: 5dc691f or older.

Heights == 1..MaxH
Idx == 1..MaxLen
Verdicts == {"none", "match", "mismatch"}

VARIABLES
    snapH,      \* height of native's snapshot the engine was built from (0 = genesis)
    input,      \* the slice: input[j] = height of the j-th ReplayBlock
    lines,      \* lines[j] = height of the block raw line j decodes to (one_pass)
    altered,    \* altered[j]: raw line j is block lines[j] by time and round, but
                \*   differs from the compared block elsewhere (actions, nonces,
                \*   signers, native responses)
    hasDiffs,   \* hasDiffs[j]: native's book diffs present for the j-th block
    i,          \* index of the block the loop is on
    pc,         \* "idle" | "executed" | "compared" | "resyncedEarly" | "halted"
    last,       \* height of the last block applied, or snapH (only the fix reads it)
    synced,     \* our book == native's book (before the current block / after resync)
    pre,        \* `synced` just before the current block was executed
    outMatch,   \* our outputs for the current block equal native's (responses + diffs)
    verdict,    \* [Idx -> Verdicts] per-block match verdict (None = reproduced)
    tOut,       \* ghost [Idx -> BOOLEAN] our own outputs of block j equalled native's
    tState,     \* ghost [Idx -> BOOLEAN] our book right after block j equalled native's
    k,          \* Replay.reproduced
    n,          \* Replay.blocks
    counted,    \* ghost: indices that incremented `reproduced`
    resynced,   \* ghost: indices passed to engine.resync
    log,        \* ghost: <<[kind |-> "snap"|"apply"|"resync", h |-> height]>>
    endState,   \* end-of-window check: "none" (not run) | "pass" | "fail"
    execAltered,\* ghost: indices whose executed line was not exactly the compared block
    claimsSI    \* the report says "state identical" for the counted blocks

vars == <<snapH, input, lines, altered, hasDiffs, i, pc, last, synced, pre, outMatch, verdict,
          tOut, tState, k, n, counted, resynced, log, endState, execAltered, claimsSI>>

new == <<altered, execAltered, claimsSI>>

Choice(flag) == IF flag THEN BOOLEAN ELSE {FALSE}

Ev(kind, h) == [kind |-> kind, h |-> h]

AnySlices == UNION {[1..len -> Heights] : len \in 1..MaxLen}
Slices(s) == IF AnyInput THEN AnySlices ELSE {[j \in 1..(MaxH - s) |-> s + j]}

(* validate(blocks, snapshot), replay.rs:149-173 at 9219ae7, with its mutations.
   Its DecodedHeight check (replay.rs:160-167) is checked in ReplayCli.tla; here
   heights are block identities (A1). *)
ValidHeight(j) ==
    CASE MUTANT = "validate_no_height_check"  -> TRUE
      [] MUTANT = "validate_ignores_snapshot" -> input[j] = j
      [] OTHER                                -> input[j] = snapH + j
ValidDiffs(j) == MUTANT = "validate_no_diffs_check" \/ hasDiffs[j]
Valid == \A j \in 1..Len(input) : ValidHeight(j) /\ ValidDiffs(j)

Init ==
    /\ snapH \in 0..(MaxH - 1)
    /\ input \in Slices(snapH)
    /\ lines \in IF LinesMayDiffer THEN [1..Len(input) -> Heights] ELSE {input}
    /\ altered \in IF LinesMayDiffer THEN [1..Len(input) -> BOOLEAN]
                                     ELSE {[j \in 1..Len(input) |-> FALSE]}
    /\ hasDiffs \in IF DiffsMayBeMissing THEN [Idx -> BOOLEAN] ELSE {[j \in Idx |-> TRUE]}
    /\ i = 1
    /\ pc = IF VALIDATE /\ ~Valid THEN "refused" ELSE "idle"   \* Err(ReplayError) before any block
    /\ last = IF MUTANT = "cursor_ignores_snapshot" THEN 0 ELSE snapH
    /\ synced = TRUE               \* engine built from native's state at snapH
    /\ pre = TRUE
    /\ outMatch = FALSE
    /\ verdict = [j \in Idx |-> "none"]
    /\ tOut = [j \in Idx |-> FALSE]
    /\ tState = [j \in Idx |-> FALSE]
    /\ k = 0 /\ n = 0
    /\ counted = {} /\ resynced = {}
    /\ log = IF snapH > 0 THEN <<Ev("snap", snapH)>> ELSE <<>>
    /\ endState = "none"
    /\ execAltered = {}
    /\ claimsSI = FALSE

(* Proposed fix: height guard. Mutations weaken it. *)
HeightOK(h) ==
    CASE MUTANT = "guard_gt" -> h > last              \* only "h > last": gaps pass
      [] MUTANT = "guard_ge" -> h >= last /\ h > 0    \* off by one: duplicates pass
      [] OTHER              -> h = last + 1

(* for b in blocks { let out = engine.execute_block(..) } *)
LineCheck == VALIDATE /\ MUTANT # "no_line_check"
(* one_pass's line check. 9219ae7: the whole decoded block equals the compared
   one (timing.rs:188). 5dc691f (V9219 = FALSE, or the mutant): time and round
   only, so a line that is the same block by time and round passes. *)
FullLineCheck == V9219 /\ MUTANT # "line_check_time_round_only"
LineSame(j) == lines[j] = input[j] /\ (FullLineCheck => ~altered[j])

Execute ==
    /\ pc = "idle"
    /\ i <= Len(input)
    /\ FIX => HeightOK(input[i])
    /\ LineCheck => LineSame(i)
    /\ \E ok, lucky \in BOOLEAN, silent \in Choice(SilentDiverge), heal \in Choice(MayHeal) :
          LET hidden == silent /\ ~ok          \* wrong, but outputs equal native's
              healed == heal /\ ~synced /\ ok  \* block overwrites the earlier difference
              outs   == (ok /\ (synced \/ lucky)) \/ hidden
              post   == (synced /\ ok) \/ healed
          IN /\ pre' = synced
             /\ synced' = post
             /\ outMatch' = outs
             /\ tOut' = [tOut EXCEPT ![i] = outs]
             /\ tState' = [tState EXCEPT ![i] = post]
    /\ last' = lines[i]
    /\ log' = Append(log, Ev("apply", lines[i]))   \* the block actually executed
    /\ pc' = IF MUTANT = "resync_before_compare" THEN "resyncedEarly" ELSE "executed"
    /\ execAltered' = IF lines[i] # input[i] \/ altered[i] THEN execAltered \cup {i} ELSE execAltered
    /\ UNCHANGED <<snapH, input, lines, hasDiffs, i, verdict, k, n, counted, resynced, endState,
                   altered, claimsSI>>

(* Proposed fix only: a block whose height is not last+1 stops the run with an error. *)
RejectHeight ==
    /\ FIX
    /\ pc = "idle"
    /\ i <= Len(input)
    /\ ~HeightOK(input[i])
    /\ pc' = "halted"
    /\ UNCHANGED <<snapH, input, lines, hasDiffs, i, last, synced, pre, outMatch, verdict,
                   tOut, tState, k, n, counted, resynced, log, endState>>
    /\ UNCHANGED new

(* one_pass at 9219ae7 (timing.rs:174-195): decode the line, execute it, stop
   the timers, then `if &block != rb.block() { return Err(Mismatch) }`. The
   line was executed on this pass's engine, but the Err leaves one_pass before
   record() and `?` in run_passes (timing.rs:138, :148) drops the pass, its
   engine and every earlier pass: nothing of it is compared, counted or
   reported. So the step is modelled as the error stop. Mutations:
   line_mismatch_continue skips the block and goes on; line_mismatch_break
   ends the pass early as if it were complete. *)
LineMismatch ==
    /\ LineCheck
    /\ pc = "idle"
    /\ i <= Len(input)
    /\ ~LineSame(i)
    /\ CASE MUTANT = "line_mismatch_continue" -> i' = i + 1 /\ pc' = "idle"
          [] MUTANT = "line_mismatch_break"    -> i' = Len(input) + 1 /\ pc' = "idle"
          [] OTHER                             -> pc' = "halted" /\ UNCHANGED i
    /\ UNCHANGED <<snapH, input, lines, hasDiffs, last, synced, pre, outMatch, verdict,
                   tOut, tState, k, n, counted, resynced, log, endState>>
    /\ UNCHANGED new

(* record(): blocks += 1; match compare(b, out) { None => reproduced += 1, .. }
   At dc993fc absent native diffs give Cause::NoNativeDiffs. Since 5dc691f compare
   uses native_diffs() = &[] when absent (replay.rs:306 at 9219ae7), so an absent entry
   matches when our diffs are empty (`looksEmpty`); validate() keeps that
   unreachable unless its diffs check is removed. *)
Compare ==
    /\ pc = "executed"
    /\ \E looksEmpty \in Choice(VALIDATE /\ ~hasDiffs[i]) :
       LET v == IF (hasDiffs[i] /\ outMatch) \/ looksEmpty THEN "match" ELSE "mismatch" IN
       /\ verdict' = [verdict EXCEPT ![i] = v]
       /\ n' = n + 1
       /\ IF v = "match" \/ MUTANT = "count_every_block"
             THEN k' = k + 1 /\ counted' = counted \cup {i}
             ELSE UNCHANGED <<k, counted>>
    /\ pc' = "compared"
    /\ UNCHANGED <<snapH, input, lines, hasDiffs, i, last, synced, pre, outMatch, tOut, tState,
                   resynced, log, endState>>
    /\ UNCHANGED new

Advance == /\ i' = i + 1 /\ pc' = "idle"

(* None arm: nothing else to do. *)
Reproduced ==
    /\ pc = "compared"
    /\ verdict[i] = "match"
    /\ Advance
    /\ UNCHANGED <<snapH, input, lines, hasDiffs, last, synced, pre, outMatch, verdict, tOut,
                   tState, k, n, counted, resynced, log, endState>>
    /\ UNCHANGED new

(* Some(m) arm: engine.resync(b.height, b.native_diffs()). With native's diffs
   for this block and our book equal to native's before the block, resync
   restores native's book (assumption A2). Without them native_diffs() is
   &[], and resync cannot restore native's changes: our book is either left
   as execute_block made it or loses this block's changes (assumption A3). *)
Resync ==
    /\ pc = "compared"
    /\ verdict[i] = "mismatch"
    /\ ~(FIX /\ ~hasDiffs[i] /\ MUTANT # "resync_without_diffs")
    /\ IF hasDiffs[i]
          THEN synced' = pre
          ELSE \E s \in BOOLEAN : synced' = (s /\ synced)
    /\ resynced' = resynced \cup {i}
    /\ log' = Append(log, Ev("resync", input[i]))
    /\ IF MUTANT = "recompare_after_resync"
          THEN pc' = "executed" /\ outMatch' = TRUE /\ UNCHANGED i
          ELSE Advance /\ UNCHANGED outMatch
    /\ UNCHANGED <<snapH, input, lines, hasDiffs, last, pre, verdict, tOut, tState, k, n, counted, endState>>
    /\ UNCHANGED new

(* Proposed fix only: native diffs missing -> stop with an error instead of resync(&[]). *)
HaltNoDiffs ==
    /\ FIX /\ MUTANT # "resync_without_diffs"
    /\ pc = "compared"
    /\ verdict[i] = "mismatch"
    /\ ~hasDiffs[i]
    /\ pc' = "halted"
    /\ UNCHANGED <<snapH, input, lines, hasDiffs, i, last, synced, pre, outMatch, verdict,
                   tOut, tState, k, n, counted, resynced, log, endState>>
    /\ UNCHANGED new

(* MUTANT resync_before_compare: resync (to native, unconditionally) runs before
   the verdict, and the verdict then compares the resynced book. *)
ResyncEarly ==
    /\ pc = "resyncedEarly"
    /\ synced' = pre
    /\ outMatch' = pre
    /\ resynced' = resynced \cup {i}
    /\ log' = Append(log, Ev("resync", input[i]))
    /\ pc' = "executed"
    /\ UNCHANGED <<snapH, input, lines, hasDiffs, i, last, pre, verdict, tOut, tState, k, n, counted, endState>>
    /\ UNCHANGED new

(* MUTANT stop_on_mismatch: `return` on the first mismatch instead of resync. *)
StopOnMismatch ==
    /\ MUTANT = "stop_on_mismatch"
    /\ pc = "compared"
    /\ verdict[i] = "mismatch"
    /\ pc' = "halted"
    /\ UNCHANGED <<snapH, input, lines, hasDiffs, i, last, synced, pre, outMatch, verdict,
                   tOut, tState, k, n, counted, resynced, log, endState>>
    /\ UNCHANGED new

Done == pc = "idle" /\ i > Len(input)

(* End-of-window check (proposed, task 2 of the headline gate): after the last
   block, compare our final book with native's state at the last height,
   translated into our book's shape. With EndExact the check fails exactly when
   the books differ (assumption A5); without it, a difference may sit in a part
   the translation drops (`blind`). Mutations: end_skip_when_all_counted skips
   the check (reports pass) when every block was counted; end_check_before_last_block
   compares the book as it was before the last block. *)
EndCheck ==
    /\ Done
    /\ \E blind \in Choice(~EndExact) :
          LET agree  == IF MUTANT = "end_check_before_last_block" THEN pre ELSE synced
              passes == IF MUTANT = "end_skip_when_all_counted" /\ k = n
                           THEN TRUE ELSE agree \/ blind
          IN endState' = IF passes THEN "pass" ELSE "fail"
    /\ pc' = "ended"
    \* The label. 5dc691f: run_passes' summary prints "state identical k/N"
    \* (timing.rs:363-367 there) and to_text does when the end check passed and
    \* k = N. 9219ae7: every printer uses verdict_label (replay.rs:182-194),
    \* which never says "state identical" (replay.rs:393, timing.rs:362,
    \* main.rs:244). Mutant label_state_identical puts the old label back.
    /\ claimsSI' = (~V9219 \/ MUTANT = "label_state_identical")
    /\ UNCHANGED <<snapH, input, lines, hasDiffs, i, last, synced, pre, outMatch, verdict,
                   tOut, tState, k, n, counted, resynced, log, altered, execAltered>>

DriverNext ==
    IF MUTANT = "stop_on_mismatch"
    THEN Execute \/ Compare \/ Reproduced \/ StopOnMismatch
    ELSE Execute \/ LineMismatch \/ RejectHeight \/ Compare \/ Reproduced \/ Resync \/ HaltNoDiffs \/ ResyncEarly
         \/ EndCheck

Spec == Init /\ [][DriverNext]_vars

LiveSpec ==
    /\ Spec
    /\ WF_vars(Execute) /\ WF_vars(Compare) /\ WF_vars(Reproduced) /\ WF_vars(Resync)
    /\ WF_vars(ResyncEarly)

(* Keeps mutated runs finite (recompare loops); never binds the code or fix
   model, whose log holds at most 1 snapshot + MaxLen applies + MaxLen resyncs. *)
LogBound == Len(log) <= 2 * MaxLen + 2

(* ---------------------------- properties --------------------------------- *)

TypeOK ==
    /\ snapH \in 0..MaxH /\ i \in 1..(MaxLen + 1) /\ last \in 0..MaxH
    /\ pc \in {"idle", "executed", "compared", "resyncedEarly", "halted", "ended", "refused"}
    /\ endState \in {"none", "pass", "fail"}
    /\ execAltered \subseteq Idx /\ claimsSI \in BOOLEAN
    /\ V9219 => VALIDATE
    /\ synced \in BOOLEAN /\ pre \in BOOLEAN /\ outMatch \in BOOLEAN
    /\ verdict \in [Idx -> Verdicts]
    /\ k \in Nat /\ n \in Nat /\ counted \subseteq Idx /\ resynced \subseteq Idx

(* Height of the latest snapshot or applied block before log position p (0 = genesis). *)
Floor(p) ==
    LET prev == {q \in 1..(p - 1) : log[q].kind \in {"snap", "apply"}}
    IN IF prev = {} THEN 0 ELSE log[CHOOSE q \in prev : \A q2 \in prev : q2 <= q].h

(* P1. Each block is applied exactly once, in height order: every applied block
   is exactly one above the previous applied block (or the snapshot). *)
P1_ExactlyOnceInOrder ==
    \A p \in 1..Len(log) : log[p].kind = "apply" => log[p].h = Floor(p) + 1

(* P1, completeness: on a well-formed slice the driver applies every block. *)
P1_AllApplied == <>Done

(* P2. A snapshot at height H is never followed by a block at or below H. *)
P2_NoBlockAtOrBelowSnapshot ==
    \A p, q \in 1..Len(log) :
        (p < q /\ log[p].kind = "snap" /\ log[q].kind = "apply") => log[q].h > log[p].h

(* P3. A block's verdict is fixed before the driver resyncs to native's book,
   it is the verdict of our own execution, and it never changes afterwards. *)
P3_VerdictBeforeResync == \A j \in resynced : verdict[j] # "none"
P3_VerdictIsOurs ==
    \A j \in Idx : verdict[j] # "none" =>
        (verdict[j] = "match" <=> (tOut[j] /\ hasDiffs[j]))
P3_VerdictNeverChanges ==
    [][\A j \in Idx : verdict[j] # "none" => verdict'[j] = verdict[j]]_vars

(* P4a. A block whose outputs mismatched native's is never counted as
   reproduced, and k <= N (k = reproduced, N = blocks). *)
P4a_OutputMismatchNeverCounted ==
    /\ \A j \in counted : tOut[j] /\ hasDiffs[j]
    /\ k = Cardinality(counted)
    /\ k <= n

(* P4b. A block counted as reproduced also left our book equal to native's
   (the report calls k/N "state_identical", timing.rs:94). *)
P4b_CountedImpliesStateIdentical ==
    \A j \in counted : tState[j]

(* E1. A passing end-of-window check means our final book equals native's. *)
E1_EndPassImpliesFinalBooksAgree == endState = "pass" => synced

(* E2. Headline N/N: every block counted (k = N) and the end check passes
   implies our book equalled native's after every block of the window. *)
E2_AllCountedAndEndPassImpliesAgreedThroughout ==
    (endState = "pass" /\ k = n) => \A j \in 1..Len(input) : tState[j]

(* E3. Partial k/N: if the end check passes, every block counted as reproduced
   left our book equal to native's (P4b, backed by the end check). *)
E3_EndPassImpliesCountedStateIdentical ==
    endState = "pass" => \A j \in counted : tState[j]

(* P5. The timed pass executes exactly the block it compares: no executed raw
   line differs from the compared block (identity or content). *)
P5_ExecutedIsCompared == execAltered = {}

(* P6. A pass that ends normally compared every block of the slice. *)
P6_FinishedPassCoversSlice == (Done \/ pc = "ended") => n = Len(input)

(* L1. If the report says "state identical", every counted block left our book
   equal to native's. *)
L1_StateIdenticalClaimHolds == claimsSI => \A j \in counted : tState[j]

(* Vacuity probes: each must be VIOLATED, showing the run reaches these states. *)
Probe_NoResync == resynced = {}
Probe_NoSnapThenApply ==
    ~\E p, q \in 1..Len(log) : p < q /\ log[p].kind = "snap" /\ log[q].kind = "apply"
Probe_NeverDoneAfterMismatch == ~(Done /\ resynced # {} /\ k > 0)
Probe_NoMismatchCounted == n = k
Probe_NeverRefused == pc # "refused"
Probe_EndNeverFails == endState # "fail"
Probe_EndNeverPassesAfterResync == ~(endState = "pass" /\ resynced # {})
\* A line that is the compared block by time and round but differs in content is reached.
Probe_NoAlteredLine == ~\E j \in 1..Len(input) : lines[j] = input[j] /\ altered[j]
=============================================================================
