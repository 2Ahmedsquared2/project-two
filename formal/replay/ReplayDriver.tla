---------------------------- MODULE ReplayDriver ----------------------------
(***************************************************************************)
(* Model of the p2-node replay driver: Replay::run / Replay::record and     *)
(* the timed loop one_pass (crates/p2-node/src/{replay,timing}.rs, lane 3,  *)
(* draft PR #10, modelled at commit dc993fc). README.md maps every action   *)
(* below to file:line, lists bounds and assumptions, and what is NOT        *)
(* covered.                                                                 *)
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
(* FIX = FALSE models the code as written. FIX = TRUE models the proposed   *)
(* fix sent to lane 3: refuse a block whose height is not last+1, and stop  *)
(* when native's diffs for a block are missing. MUTANT selects one          *)
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
    MUTANT             \* "none" or a mutation name from the README

Heights == 1..MaxH
Idx == 1..MaxLen
Verdicts == {"none", "match", "mismatch"}

VARIABLES
    snapH,      \* height of native's snapshot the engine was built from (0 = genesis)
    input,      \* the slice: input[j] = height of the j-th ReplayBlock
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
    log         \* ghost: <<[kind |-> "snap"|"apply"|"resync", h |-> height]>>

vars == <<snapH, input, hasDiffs, i, pc, last, synced, pre, outMatch, verdict,
          tOut, tState, k, n, counted, resynced, log>>

Ev(kind, h) == [kind |-> kind, h |-> h]

AnySlices == UNION {[1..len -> Heights] : len \in 1..MaxLen}
Slices(s) == IF AnyInput THEN AnySlices ELSE {[j \in 1..(MaxH - s) |-> s + j]}

Init ==
    /\ snapH \in 0..(MaxH - 1)
    /\ input \in Slices(snapH)
    /\ hasDiffs \in IF DiffsMayBeMissing THEN [Idx -> BOOLEAN] ELSE {[j \in Idx |-> TRUE]}
    /\ i = 1
    /\ pc = "idle"
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

(* Proposed fix: height guard. Mutations weaken it. *)
HeightOK(h) ==
    CASE MUTANT = "guard_gt" -> h > last              \* only "h > last": gaps pass
      [] MUTANT = "guard_ge" -> h >= last /\ h > 0    \* off by one: duplicates pass
      [] OTHER              -> h = last + 1

(* for b in blocks { let out = engine.execute_block(..) } *)
Execute ==
    /\ pc = "idle"
    /\ i <= Len(input)
    /\ FIX => HeightOK(input[i])
    /\ \E ok, lucky \in BOOLEAN :
          /\ pre' = synced
          /\ synced' = (synced /\ ok)
          /\ outMatch' = (ok /\ (synced \/ lucky))
          /\ tOut' = [tOut EXCEPT ![i] = ok /\ (synced \/ lucky)]
          /\ tState' = [tState EXCEPT ![i] = synced /\ ok]
    /\ last' = input[i]
    /\ log' = Append(log, Ev("apply", input[i]))
    /\ pc' = IF MUTANT = "resync_before_compare" THEN "resyncedEarly" ELSE "executed"
    /\ UNCHANGED <<snapH, input, hasDiffs, i, verdict, k, n, counted, resynced>>

(* Proposed fix only: a block whose height is not last+1 stops the run with an error. *)
RejectHeight ==
    /\ FIX
    /\ pc = "idle"
    /\ i <= Len(input)
    /\ ~HeightOK(input[i])
    /\ pc' = "halted"
    /\ UNCHANGED <<snapH, input, hasDiffs, i, last, synced, pre, outMatch, verdict,
                   tOut, tState, k, n, counted, resynced, log>>

(* record(): blocks += 1; match compare(b, out) { None => reproduced += 1, .. } *)
Compare ==
    /\ pc = "executed"
    /\ LET v == IF hasDiffs[i] /\ outMatch THEN "match" ELSE "mismatch" IN
       /\ verdict' = [verdict EXCEPT ![i] = v]
       /\ n' = n + 1
       /\ IF v = "match" \/ MUTANT = "count_every_block"
             THEN k' = k + 1 /\ counted' = counted \cup {i}
             ELSE UNCHANGED <<k, counted>>
    /\ pc' = "compared"
    /\ UNCHANGED <<snapH, input, hasDiffs, i, last, synced, pre, outMatch, tOut, tState,
                   resynced, log>>

Advance == /\ i' = i + 1 /\ pc' = "idle"

(* None arm: nothing else to do. *)
Reproduced ==
    /\ pc = "compared"
    /\ verdict[i] = "match"
    /\ Advance
    /\ UNCHANGED <<snapH, input, hasDiffs, last, synced, pre, outMatch, verdict, tOut,
                   tState, k, n, counted, resynced, log>>

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
    /\ UNCHANGED <<snapH, input, hasDiffs, last, pre, verdict, tOut, tState, k, n, counted>>

(* Proposed fix only: native diffs missing -> stop with an error instead of resync(&[]). *)
HaltNoDiffs ==
    /\ FIX /\ MUTANT # "resync_without_diffs"
    /\ pc = "compared"
    /\ verdict[i] = "mismatch"
    /\ ~hasDiffs[i]
    /\ pc' = "halted"
    /\ UNCHANGED <<snapH, input, hasDiffs, i, last, synced, pre, outMatch, verdict,
                   tOut, tState, k, n, counted, resynced, log>>

(* MUTANT resync_before_compare: resync (to native, unconditionally) runs before
   the verdict, and the verdict then compares the resynced book. *)
ResyncEarly ==
    /\ pc = "resyncedEarly"
    /\ synced' = pre
    /\ outMatch' = pre
    /\ resynced' = resynced \cup {i}
    /\ log' = Append(log, Ev("resync", input[i]))
    /\ pc' = "executed"
    /\ UNCHANGED <<snapH, input, hasDiffs, i, last, pre, verdict, tOut, tState, k, n, counted>>

(* MUTANT stop_on_mismatch: `return` on the first mismatch instead of resync. *)
StopOnMismatch ==
    /\ MUTANT = "stop_on_mismatch"
    /\ pc = "compared"
    /\ verdict[i] = "mismatch"
    /\ pc' = "halted"
    /\ UNCHANGED <<snapH, input, hasDiffs, i, last, synced, pre, outMatch, verdict,
                   tOut, tState, k, n, counted, resynced, log>>

DriverNext ==
    IF MUTANT = "stop_on_mismatch"
    THEN Execute \/ Compare \/ Reproduced \/ StopOnMismatch
    ELSE Execute \/ RejectHeight \/ Compare \/ Reproduced \/ Resync \/ HaltNoDiffs \/ ResyncEarly

Done == pc = "idle" /\ i > Len(input)

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
    /\ pc \in {"idle", "executed", "compared", "resyncedEarly", "halted"}
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

(* Vacuity probes: each must be VIOLATED, showing the run reaches these states. *)
Probe_NoResync == resynced = {}
Probe_NoSnapThenApply ==
    ~\E p, q \in 1..Len(log) : p < q /\ log[p].kind = "snap" /\ log[q].kind = "apply"
Probe_NeverDoneAfterMismatch == ~(Done /\ resynced # {} /\ k > 0)
Probe_NoMismatchCounted == n = k
=============================================================================
