----------------------------- MODULE ReplayCli -----------------------------
(***************************************************************************)
(* The p2-node replay CLI's slice construction (crates/p2-node/src/main.rs, *)
(* lane 3, PR #10, sync commit 9219ae7): line i of the replica_cmds file is *)
(* given height snapshot + 1 + i by position, so validate()'s height check  *)
(* holds by construction. What refuses a dropped, repeated, swapped or      *)
(* stale line is ReplayBlock::new: the line's block time (ms) must equal    *)
(* the block time of native's diffs entry at the assumed height             *)
(* (Misaligned, fatal), and validate() needs diffs for every height.        *)
(* At 9219ae7 validate() also refuses a line that carries its own height    *)
(* (top-level `height`, Block.height) when that height is not the assumed   *)
(* one (ReplayError::DecodedHeight). Lines without the field are checked by *)
(* block time only, as at 5dc691f. HeightField picks which lines carry it.  *)
(*                                                                         *)
(* Question: if the load is accepted, is every line really native's block   *)
(* at its assumed height? Then the driver model's `input` is the true block *)
(* sequence and ReplayDriver's P1/P2 results carry over to the CLI path.    *)
(*                                                                         *)
(* Native block times T: heights -> ms, nondecreasing. StrictTimes = TRUE   *)
(* assumes strictly increasing (no two heights share a ms block time).      *)
(* Diffs exist for heights 1..MaxH only (a later height has none).          *)
(***************************************************************************)
EXTENDS Naturals, Sequences

CONSTANTS
    MaxH,         \* native heights with diffs: 1..MaxH
    MaxLen,       \* longest replica file (lines)
    StrictTimes,  \* TRUE: native block times strictly increase with height
    HeightField,  \* "never" (5dc691f, or a line without the field) | "always" | "any" (per line)
    MUTANT        \* "none" | "no_alignment" | "align_first_line_only"
                  \*   | "no_decoded_height_check" | "decoded_height_ignores_snapshot"

Heights == 1..MaxH
Times == 1..(MaxH + 2)   \* ms values; small but leaves room for gaps
Mono(t) == \A h \in 1..(MaxH - 1) : IF StrictTimes THEN t[h] < t[h + 1] ELSE t[h] <= t[h + 1]

VARIABLES snapH, lines, carries, T, accepted

vars == <<snapH, lines, carries, T, accepted>>

\* main.rs:168: h = snapshot + 1 + i (index i from 0; here j from 1).
Assumed(j) == snapH + j

\* ReplayBlock::new (replay.rs:60-67) plus validate()'s diffs check
\* (replay.rs:168-170): diffs present at the assumed height and block times equal.
LineOK(j) ==
    /\ Assumed(j) \in Heights
    /\ \/ MUTANT = "no_alignment"
       \/ MUTANT = "align_first_line_only" /\ j > 1
       \/ T[lines[j]] = T[Assumed(j)]

\* validate()'s DecodedHeight check (replay.rs:160-167): a line that carries
\* a height must carry the assumed one. A carried height is the block's own
\* height (assumption A8), so it equals Assumed(j) exactly when lines[j] does.
DecodedOK(j) ==
    CASE MUTANT = "no_decoded_height_check"         -> TRUE
      [] ~carries[j]                                -> TRUE
      [] MUTANT = "decoded_height_ignores_snapshot" -> lines[j] = j
      [] OTHER                                      -> lines[j] = Assumed(j)

CarrySets(len) ==
    CASE HeightField = "never"  -> {[j \in 1..len |-> FALSE]}
      [] HeightField = "always" -> {[j \in 1..len |-> TRUE]}
      [] OTHER                  -> [1..len -> BOOLEAN]

Init ==
    /\ snapH \in 0..(MaxH - 1)
    /\ lines \in UNION {[1..len -> Heights] : len \in 1..MaxLen}
    /\ carries \in CarrySets(Len(lines))
    /\ T \in {t \in [Heights -> Times] : Mono(t)}
    /\ accepted = \A j \in 1..Len(lines) : LineOK(j) /\ DecodedOK(j)

Spec == Init /\ [][FALSE]_vars

(* CLI1. An accepted load executes exactly native's blocks snapshot+1, +2, ...
   in order: every line is native's block at its assumed height. *)
CLI1_AcceptedLinesAreTheirHeights ==
    accepted => \A j \in 1..Len(lines) : lines[j] = Assumed(j)

(* CLI2. The checks refuse no correct load: a file that is exactly native's
   blocks snapshot+1, +2, ... (all with diffs) is accepted. *)
CLI2_CorrectLoadAccepted ==
    (\A j \in 1..Len(lines) : lines[j] = Assumed(j) /\ Assumed(j) \in Heights) => accepted

(* Vacuity probes: each must be VIOLATED. *)
Probe_NeverAccepted == ~accepted
Probe_NeverRefused == accepted
Probe_NeverAcceptedAfterSnapshot == ~(accepted /\ snapH > 0 /\ Len(lines) > 1)
\* Some load is refused by the decoded-height check alone (its block times align).
Probe_NoDecodedHeightOnlyRefusal ==
    ~(~accepted /\ \A j \in 1..Len(lines) : LineOK(j))
=============================================================================
