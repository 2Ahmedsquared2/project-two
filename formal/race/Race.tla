---- MODULE Race ----
(* The ingest racer: several sources deliver the same keyed stream, the
   racer releases the first copy of each key once, in key order, and raises
   an alarm for a copy whose bytes differ from the copy it holds.

   Follows crates/hl-race/src/lib.rs: Racer::offer (window classification,
   first copy wins, duplicate/divergent compare, late and too-far drops),
   Racer::pop (release of the next key), and the slot store `slots`
   (state Empty/Held/Released, seq, bytes). The arithmetic of
   window::classify (slot = seq % W, Ahead iff next <= seq < next + W) is
   used here as written; Kani checks the Rust function itself.
   Source read at 06f7763 (main) plus this branch's hl-race.

   Sources. Each source walks the keys 0..N-1 in increasing order (an
   hl-node by-block file and a by-block relay are in height order; measured
   on 50,218 real lines, docs/proofs/race.md). Sources interleave
   arbitrarily, so copies of one key reach the racer in any order across
   sources and with any delay. A source loses the keys in Lost, sends a
   second copy of the keys in Dup, and sends different bytes for the keys in
   Bad. Values are "ok" or "bad".

   Not modelled: Racer::skip_missing (the caller's give-up policy; the model
   has none, so a key is released only after it arrives), payload size
   limits, timing statistics, u64 wraparound (Kani), a source that reorders
   its own stream.

   Driver contract (found by TLC, see Send): drain pop() before the next
   offer().

   Assumptions: every source step and the release step are weakly fair;
   liveness is checked only in runs where at least one source loses
   nothing. *)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS S, N, W, Lost, Dup, Bad
\* Sources are 1..S. Lost, Dup and Bad are sets of codes 10 * source + key
\* (TLC configuration files cannot write tuples); N <= 10.
Sources == 1..S
Code(s, k) == 10 * s + k

Keys == 0..(N - 1)
Slots == 0..(W - 1)
Val(s, k) == IF Code(s, k) \in Bad THEN "bad" ELSE "ok"

VARIABLES pos, duped, next, slot, out, alarms, won, cmp, acc
srcs == <<pos, duped>>
racer == <<next, slot, out, alarms>>
ghost == <<won, cmp, acc>>
vars == <<srcs, racer, ghost>>

EmptySlot == [st |-> "empty", seq |-> 0, val |-> "ok"]

Init == /\ pos = [s \in Sources |-> 0]
        /\ duped = {}
        /\ next = 0
        /\ slot = [i \in Slots |-> EmptySlot]
        /\ out = <<>>
        /\ alarms = {}
        /\ won = [k \in Keys |-> "none"]
        /\ cmp = {}
        /\ acc = {}

\* Ghost bookkeeping, written independently of the racer's branches so a
\* bug in a branch cannot also hide itself from the properties:
\* won[k] is the value of the first copy the racer accepted for k (set
\* once); cmp holds every copy offered while the slot of k still held k.
Comparable(k) == slot[k % W].st # "empty" /\ slot[k % W].seq = k
Ghost(s, k, v) ==
  /\ cmp' = IF Comparable(k) THEN cmp \cup {<<s, k, v>>} ELSE cmp
  /\ won' = IF won[k] = "none" /\ ~Comparable(k) /\ next <= k /\ k < next + W
           THEN [won EXCEPT ![k] = v] ELSE won
  \* acc: keys the racer itself stored (read from its new slot state).
  /\ acc' = IF slot'[k % W] = [st |-> "held", seq |-> k, val |-> v] /\ slot[k % W] # slot'[k % W]
           THEN acc \cup {k} ELSE acc

\* Racer::offer for one copy of key k from source s with value v.
OfferRacer(s, k, v) ==
  LET i == k % W IN
  IF k >= next + W
  THEN UNCHANGED racer                                         \* TooFar
  ELSE IF slot[i].st # "empty" /\ slot[i].seq = k
  THEN /\ IF slot[i].val # v
          THEN alarms' = alarms \cup {<<s, k>>}                \* Divergent
          ELSE UNCHANGED alarms                                \* Duplicate
       /\ UNCHANGED <<next, slot, out>>
  ELSE IF k < next
  THEN UNCHANGED racer                                         \* Late
  ELSE /\ slot' = [slot EXCEPT ![i] = [st |-> "held", seq |-> k, val |-> v]]
       /\ UNCHANGED <<next, out, alarms>>                      \* Won

Offer(s, k, v) == OfferRacer(s, k, v) /\ Ghost(s, k, v)

\* A source sends its next key: lost, or offered (and offered again once
\* when the key is in Dup).
\* The driver drains every releasable key before the next offer, as
\* drain_round_robin does (lib.rs: `while let Some(r) = racer.pop()` after
\* each offer). TLC showed this is required for liveness: offering key
\* next + W from the only complete source while key next is still held
\* drops it as TooFar, and that source never resends it.
CanPop == LET i == next % W IN
          next < N /\ slot[i].st = "held" /\ slot[i].seq = next
Send(s) ==
  LET k == pos[s] IN
  /\ ~CanPop
  /\ k < N
  /\ IF Code(s, k) \in Lost
     THEN /\ pos' = [pos EXCEPT ![s] = k + 1]
          /\ UNCHANGED <<duped, racer, ghost>>
     ELSE /\ Offer(s, k, Val(s, k))
          /\ IF Code(s, k) \in Dup /\ <<s, k>> \notin duped
             THEN /\ duped' = duped \cup {<<s, k>>}
                  /\ UNCHANGED pos
             ELSE /\ pos' = [pos EXCEPT ![s] = k + 1]
                  /\ UNCHANGED duped

\* Racer::pop.
Pop ==
  LET i == next % W IN
  /\ next < N
  /\ slot[i].st = "held"
  /\ slot[i].seq = next
  /\ slot' = [slot EXCEPT ![i].st = "released"]
  /\ out' = Append(out, <<next, slot[i].val>>)
  /\ next' = next + 1
  /\ UNCHANGED <<srcs, alarms, ghost>>

Done == /\ \A s \in Sources : pos[s] = N
        /\ UNCHANGED vars

Next == (\E s \in Sources : Send(s)) \/ Pop \/ Done

Spec == Init /\ [][Next]_vars /\ (\A s \in Sources : WF_vars(Send(s))) /\ WF_vars(Pop)

TypeOK == /\ pos \in [Sources -> 0..N]
          /\ next \in 0..N
          /\ \A i \in Slots : slot[i].st \in {"empty", "held", "released"}
          /\ alarms \subseteq Sources \X Keys

\* Each key is released at most once, in key order, with no key skipped.
InOrderOnce == \A j \in 1..Len(out) : out[j][1] = j - 1
\* What is released is the first copy that won the key.
ReleasedIsWinner == \A j \in 1..Len(out) : out[j][2] = won[out[j][1]]
\* Every copy compared against a held or released copy with different bytes
\* raised the alarm: the racer never silently keeps the first.
DivergenceAlarmed == \A c \in cmp : c[3] # won[c[2]] => <<c[1], c[2]>> \in alarms
\* No alarm without a real difference.
NoFalseAlarm == \A a \in alarms : Val(a[1], a[2]) # won[a[2]]
\* A run where one source sends a different value than the winning one for
\* a key it delivers while the winner is held raises the alarm.
Alarmed == alarms # {}
\* Liveness: when one source loses nothing, every key is released.
AllReleased == <>(next = N)
\* A key accepted and not yet released is still in its slot: a later
\* arrival never evicts it.
HeldNotLost == \A k \in acc : k >= next =>
                 (slot[k % W].st = "held" /\ slot[k % W].seq = k)
\* Reach: the window is full of held keys ahead of a missing one.
GapHeld == \E i \in Slots : slot[i].st = "held" /\ slot[i].seq > next
====
