----------------------------- MODULE ReplayCli -----------------------------
(***************************************************************************)
(* The p2-node replay CLI's slice construction (crates/p2-node/src/main.rs, *)
(* lane 3, PR #10, sync commit 5dc691f): line i of the replica_cmds file is *)
(* given height snapshot + 1 + i by position, so validate()'s height check  *)
(* holds by construction. What refuses a dropped, repeated, swapped or      *)
(* stale line is ReplayBlock::new: the line's block time (ms) must equal    *)
(* the block time of native's diffs entry at the assumed height             *)
(* (Misaligned, fatal), and validate() needs diffs for every height.        *)
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
    MUTANT        \* "none" | "no_alignment" | "align_first_line_only"

Heights == 1..MaxH
Times == 1..(MaxH + 2)   \* ms values; small but leaves room for gaps
Mono(t) == \A h \in 1..(MaxH - 1) : IF StrictTimes THEN t[h] < t[h + 1] ELSE t[h] <= t[h + 1]

VARIABLES snapH, lines, T, accepted

vars == <<snapH, lines, T, accepted>>

\* main.rs:165: h = snapshot + 1 + i (index i from 0; here j from 1).
Assumed(j) == snapH + j

\* ReplayBlock::new (replay.rs:60-67) plus validate()'s diffs check
\* (replay.rs:161-163): diffs present at the assumed height and block times equal.
LineOK(j) ==
    /\ Assumed(j) \in Heights
    /\ \/ MUTANT = "no_alignment"
       \/ MUTANT = "align_first_line_only" /\ j > 1
       \/ T[lines[j]] = T[Assumed(j)]

Init ==
    /\ snapH \in 0..(MaxH - 1)
    /\ lines \in UNION {[1..len -> Heights] : len \in 1..MaxLen}
    /\ T \in {t \in [Heights -> Times] : Mono(t)}
    /\ accepted = \A j \in 1..Len(lines) : LineOK(j)

Spec == Init /\ [][FALSE]_vars

(* CLI1. An accepted load executes exactly native's blocks snapshot+1, +2, ...
   in order: every line is native's block at its assumed height. *)
CLI1_AcceptedLinesAreTheirHeights ==
    accepted => \A j \in 1..Len(lines) : lines[j] = Assumed(j)

(* Vacuity probes: each must be VIOLATED. *)
Probe_NeverAccepted == ~accepted
Probe_NeverRefused == accepted
Probe_NeverAcceptedAfterSnapshot == ~(accepted /\ snapH > 0 /\ Len(lines) > 1)
=============================================================================
