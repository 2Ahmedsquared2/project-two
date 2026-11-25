/-!
# HlBook: the `hl-exec` order book as a pure model

Follows `crates/hl-exec/src/book.rs` (Source read at 9d7d54c):

* `Book::rest` (back of a level, or in front of an anchor: `restAt`),
  `Book::remove`, `Book::resize`, `Book::match_taker` (one fill per step:
  `match1`), and the per-level `total` / `count` they update.
* The Rust book keeps, per side and price, a level with `total` (sum of its
  orders' sizes) and `count` (number of orders); a level is dropped when its
  count reaches 0. That is the incremental L2 view. Here it is `View`: a
  function from `(side, price)` to `Option (total, count)`, `none` = no level.

Simplifications (the model is not the code):

* The arena, free lists, intrusive links and the sorted ladder are replaced
  by one list of orders in queue order (FIFO within a key = list order).
  The ladder's sortedness and slot arithmetic are Kani's (Proof Reason,
  `formal/kani-engine`), not this model's.
* Prices and sizes are `Nat` (Rust: `u64`). Rust rejects a level total above
  `u64::MAX` (`LevelOverflow`); the model has no bound, so the theorems hold
  for every input where Rust does not take that error branch.
* Checks the Rust code performs before a step (zero size, duplicate oid,
  crossing, anchor side/price) are the `Option` guards below or theorem
  hypotheses.

Properties proved for every size:

1. `rest_inv`, `restAt_inv`, `cancel_inv`, `resize_inv`, `match1_inv`: each
   step keeps `Inv`: the incremental view equals the view recomputed from the
   book at every key, and no level with count 0 stays.
2. `match1_priority`: the maker a fill hits is on the opposite side, no
   opposite order has a better price, and no order ahead of it in the queue
   has its side and price (price-time priority).
-/

namespace HlBook

structure Order where
  oid : Nat
  bid : Bool
  px : Nat
  sz : Nat
deriving Repr, DecidableEq

abbrev Key := Bool × Nat

def Order.key (o : Order) : Key := (o.bid, o.px)

/-- Sum of sizes at a key (recomputed L2 total). -/
def tot (k : Key) : List Order → Nat
  | [] => 0
  | o :: os => (if o.key = k then o.sz else 0) + tot k os

/-- Number of orders at a key (recomputed L2 count). -/
def cnt (k : Key) : List Order → Nat
  | [] => 0
  | o :: os => (if o.key = k then 1 else 0) + cnt k os

abbrev View := Key → Option (Nat × Nat)

/-- The invariant: the incremental view is the recomputed one, and a key has
a level exactly when some order rests there. -/
def Inv (b : List Order) (v : View) : Prop :=
  ∀ k, v k = if cnt k b = 0 then none else some (tot k b, cnt k b)

def update (v : View) (k : Key) (x : Option (Nat × Nat)) : View :=
  fun k' => if k' = k then x else v k'

/-- Remove the first order with `oid`, returning it. -/
def take (oid : Nat) : List Order → Option (Order × List Order)
  | [] => none
  | o :: os =>
    if o.oid = oid then some (o, os)
    else match take oid os with
      | some (x, r) => some (x, o :: r)
      | none => none

/-- Set the size of the first order with `oid`. -/
def setSz (oid sz : Nat) : List Order → List Order
  | [] => []
  | o :: os => if o.oid = oid then { o with sz := sz } :: os else o :: setSz oid sz os

/-- Level after an order of size `sz` joins it (book.rs `rest`: a new level
starts at (sz, 1), else total += sz, count += 1). -/
def addLevel (x : Option (Nat × Nat)) (sz : Nat) : Option (Nat × Nat) :=
  match x with
  | none => some (sz, 1)
  | some (t, c) => some (t + sz, c + 1)

/-- Level after an order of size `sz` leaves it (book.rs `unlink`: total -=
sz, count -= 1, a level whose count was 1 is dropped). -/
def subLevel (x : Option (Nat × Nat)) (sz : Nat) : Option (Nat × Nat) :=
  match x with
  | none => none
  | some (t, c) => if c ≤ 1 then none else some (t - sz, c - 1)

/-- Level after an order's size changes from `old` to `new` (book.rs `resize`). -/
def setLevel (x : Option (Nat × Nat)) (old new : Nat) : Option (Nat × Nat) :=
  match x with
  | none => none
  | some (t, c) => some (t - old + new, c)

/-- `Book::rest` with no anchor: back of its level. -/
def rest (o : Order) (b : List Order) (v : View) : Option (List Order × View) :=
  if o.sz = 0 then none else some (b ++ [o], update v o.key (addLevel (v o.key) o.sz))

/-- `Book::rest` with an anchor (`insertBefore`): queue position `i`. -/
def restAt (i : Nat) (o : Order) (b : List Order) (v : View) : Option (List Order × View) :=
  if o.sz = 0 then none else some (b.insertIdx i o, update v o.key (addLevel (v o.key) o.sz))

/-- `Book::remove`. -/
def cancel (oid : Nat) (b : List Order) (v : View) : Option (List Order × View) :=
  (take oid b).map fun (o, r) => (r, update v o.key (subLevel (v o.key) o.sz))

/-- `Book::resize`. -/
def resize (oid sz : Nat) (b : List Order) (v : View) : Option (List Order × View) :=
  if sz = 0 then none
  else (take oid b).map fun (o, _) => (setSz oid sz b, update v o.key (setLevel (v o.key) o.sz sz))

/-- Is `a` a better price than `b` for the side `bid` rests on? -/
def better (bid : Bool) (a b : Nat) : Bool := if bid then b < a else a < b

/-- Best price among resting orders on side `bid`. -/
def best (bid : Bool) : List Order → Option Nat
  | [] => none
  | o :: os =>
    match best bid os with
    | none => if o.bid = bid then some o.px else none
    | some p => if o.bid = bid ∧ better bid o.px p then some o.px else some p

/-- Does a taker on side `tbid` at `tpx` cross price `p`? -/
def crossesP (tbid : Bool) (tpx p : Nat) : Bool := if tbid then p ≤ tpx else tpx ≤ p

/-- The maker a taker hits: the first order in queue order on the opposite
side at the opposite best price, if the taker crosses it. -/
def pick (tbid : Bool) (tpx : Nat) (b : List Order) : Option Order :=
  (best (!tbid) b).bind fun p =>
    if crossesP tbid tpx p then b.find? (fun o => o.bid = !tbid ∧ o.px = p) else none

/-- One fill of `tsz` against maker `m` at its price: remove it when it is
used up, else resize it. -/
def fill (m : Order) (tsz : Nat) (b : List Order) (v : View) : Option (List Order × View) :=
  if min tsz m.sz = m.sz then cancel m.oid b v else resize m.oid (m.sz - min tsz m.sz) b v

/-- One step of book.rs `match_taker` (self-trade branch not modelled). -/
def match1 (tbid : Bool) (tpx tsz : Nat) (b : List Order) (v : View) :
    Option (Order × List Order × View) :=
  if tsz = 0 then none
  else (pick tbid tpx b).bind fun m => (fill m tsz b v).map fun (b', v') => (m, b', v')

/-! ## Lemmas -/

private theorem tot_append (k : Key) (a c : List Order) : tot k (a ++ c) = tot k a + tot k c := by
  induction a with
  | nil => simp [tot]
  | cons o os ih => simp only [List.cons_append, tot, ih]; omega

private theorem cnt_append (k : Key) (a c : List Order) : cnt k (a ++ c) = cnt k a + cnt k c := by
  induction a with
  | nil => simp [cnt]
  | cons o os ih => simp only [List.cons_append, cnt, ih]; omega

private theorem tot_insertIdx (k : Key) (i : Nat) (o : Order) (b : List Order) (h : i ≤ b.length) :
    tot k (b.insertIdx i o) = tot k b + (if o.key = k then o.sz else 0) := by
  induction b generalizing i with
  | nil => simp at h; subst h; simp [tot]
  | cons x xs ih =>
    cases i with
    | zero => simp only [List.insertIdx_zero, tot]; omega
    | succ j =>
      simp only [List.insertIdx_succ_cons, tot]
      rw [ih j (by simp at h; omega)]; omega

private theorem cnt_insertIdx (k : Key) (i : Nat) (o : Order) (b : List Order) (h : i ≤ b.length) :
    cnt k (b.insertIdx i o) = cnt k b + (if o.key = k then 1 else 0) := by
  induction b generalizing i with
  | nil => simp at h; subst h; simp [cnt]
  | cons x xs ih =>
    cases i with
    | zero => simp only [List.insertIdx_zero, cnt]; omega
    | succ j =>
      simp only [List.insertIdx_succ_cons, cnt]
      rw [ih j (by simp at h; omega)]; omega

private theorem take_spec (k : Key) (oid : Nat) (b r : List Order) (o : Order)
    (h : take oid b = some (o, r)) :
    tot k b = tot k r + (if o.key = k then o.sz else 0) ∧
    cnt k b = cnt k r + (if o.key = k then 1 else 0) ∧
    (∀ sz, tot k (setSz oid sz b) + (if o.key = k then o.sz else 0)
        = tot k b + (if o.key = k then sz else 0)) ∧
    (∀ sz, cnt k (setSz oid sz b) = cnt k b) := by
  induction b generalizing r with
  | nil => simp [take] at h
  | cons x xs ih =>
    unfold take at h
    by_cases hx : x.oid = oid
    · simp only [hx, ite_true, Option.some.injEq, Prod.mk.injEq] at h
      obtain ⟨rfl, rfl⟩ := h
      refine ⟨by simp only [tot]; omega, by simp only [cnt]; omega, ?_, ?_⟩ <;>
        intro sz <;> simp only [setSz, hx, ite_true, tot, cnt] <;>
        by_cases hk : (x.bid, x.px) = k <;> simp [Order.key, hk] <;> omega
    · simp only [hx, ite_false] at h
      cases hr : take oid xs with
      | none => simp [hr] at h
      | some p =>
        obtain ⟨y, r'⟩ := p
        simp only [hr, Option.some.injEq, Prod.mk.injEq] at h
        obtain ⟨rfl, rfl⟩ := h
        obtain ⟨h1, h2, h3, h4⟩ := ih r' hr
        refine ⟨by simp only [tot]; omega, by simp only [cnt]; omega, ?_, ?_⟩ <;>
          intro sz <;> simp only [setSz, hx, ite_false, tot, cnt]
        · have := h3 sz; omega
        · have := h4 sz; omega

/-- The view entry at `k` is the recomputed one for book `b`. -/
def At (b : List Order) (x : Option (Nat × Nat)) (k : Key) : Prop :=
  x = if cnt k b = 0 then none else some (tot k b, cnt k b)

private theorem inv_update (b b' : List Order) (v : View) (k0 : Key) (x : Option (Nat × Nat))
    (hv : Inv b v) (hk : At b' x k0)
    (hother : ∀ k, k ≠ k0 → tot k b' = tot k b ∧ cnt k b' = cnt k b) :
    Inv b' (update v k0 x) := by
  intro k
  unfold update
  by_cases e : k = k0
  · subst e; simpa [At] using hk
  · simp only [e, ite_false]; rw [hv k, (hother k e).1, (hother k e).2]

private theorem cnt_zero_tot (k : Key) (b : List Order) (h : cnt k b = 0) : tot k b = 0 := by
  induction b with
  | nil => rfl
  | cons x xs ih =>
    simp only [cnt, tot] at *
    by_cases hk : x.key = k <;> simp [hk] at h ⊢ <;> exact ih (by omega)

private theorem addLevel_at (b b' : List Order) (k : Key) (sz : Nat) (x : Option (Nat × Nat))
    (hx : At b x k) (ht : tot k b' = tot k b + sz) (hc : cnt k b' = cnt k b + 1) :
    At b' (addLevel x sz) k := by
  unfold At at *; subst hx
  by_cases h0 : cnt k b = 0
  · have := cnt_zero_tot k b h0; simp [h0, addLevel, ht, hc]; omega
  · simp [h0, addLevel, ht, hc]

private theorem subLevel_at (b b' : List Order) (k : Key) (sz : Nat) (x : Option (Nat × Nat))
    (hx : At b x k) (ht : tot k b = tot k b' + sz) (hc : cnt k b = cnt k b' + 1) :
    At b' (subLevel x sz) k := by
  unfold At at *; subst hx
  have h0 : cnt k b ≠ 0 := by omega
  simp only [h0, ite_false, subLevel]
  by_cases h1 : cnt k b ≤ 1
  · have : cnt k b' = 0 := by omega
    simp [h1, this]
  · have : cnt k b' ≠ 0 := by omega
    simp only [h1, ite_false, this, Option.some.injEq, Prod.mk.injEq]; omega

private theorem setLevel_at (b b' : List Order) (k : Key) (old new : Nat) (x : Option (Nat × Nat))
    (hx : At b x k) (hpos : cnt k b ≠ 0) (hold : old ≤ tot k b)
    (ht : tot k b' + old = tot k b + new) (hc : cnt k b' = cnt k b) :
    At b' (setLevel x old new) k := by
  unfold At at *; subst hx
  rw [hc]
  simp only [hpos, ite_false, setLevel, Option.some.injEq, Prod.mk.injEq, and_true]; omega

/-! ## Invariant preservation -/

theorem rest_inv (o : Order) (b b' : List Order) (v v' : View)
    (hv : Inv b v) (h : rest o b v = some (b', v')) : Inv b' v' := by
  unfold rest at h
  by_cases hz : o.sz = 0
  · simp [hz] at h
  · simp only [hz, ite_false, Option.some.injEq, Prod.mk.injEq] at h
    obtain ⟨rfl, rfl⟩ := h
    apply inv_update b _ v o.key _ hv
    · exact addLevel_at b _ o.key o.sz _ (hv o.key)
        (by rw [tot_append]; simp [tot]) (by rw [cnt_append]; simp [cnt])
    · intro k hk
      have hne : ¬ o.key = k := fun e => hk e.symm
      simp [tot_append, cnt_append, tot, cnt, hne]

private theorem rest_inv.witness : ∃ (o : Order) (b b' : List Order) (v v' : View),
    Inv b v ∧ rest o b v = some (b', v') :=
  ⟨⟨1, true, 5, 2⟩, [], _, fun _ => none, _, by intro k; simp [cnt], rfl⟩

theorem restAt_inv (i : Nat) (o : Order) (b b' : List Order) (v v' : View) (hi : i ≤ b.length)
    (hv : Inv b v) (h : restAt i o b v = some (b', v')) : Inv b' v' := by
  unfold restAt at h
  by_cases hz : o.sz = 0
  · simp [hz] at h
  · simp only [hz, ite_false, Option.some.injEq, Prod.mk.injEq] at h
    obtain ⟨rfl, rfl⟩ := h
    apply inv_update b _ v o.key _ hv
    · exact addLevel_at b _ o.key o.sz _ (hv o.key)
        (by rw [tot_insertIdx _ _ _ _ hi]; simp) (by rw [cnt_insertIdx _ _ _ _ hi]; simp)
    · intro k hk
      have hne : ¬ o.key = k := fun e => hk e.symm
      simp [tot_insertIdx _ _ _ _ hi, cnt_insertIdx _ _ _ _ hi, hne]

private theorem restAt_inv.witness : ∃ (i : Nat) (o : Order) (b b' : List Order) (v v' : View),
    i ≤ b.length ∧ Inv b v ∧ restAt i o b v = some (b', v') :=
  ⟨0, ⟨1, true, 5, 2⟩, [], _, fun _ => none, _, by simp, by intro k; simp [cnt], rfl⟩

theorem cancel_inv (oid : Nat) (b b' : List Order) (v v' : View)
    (hv : Inv b v) (h : cancel oid b v = some (b', v')) : Inv b' v' := by
  unfold cancel at h
  cases ht : take oid b with
  | none => simp [ht] at h
  | some p =>
    obtain ⟨o, r⟩ := p
    simp only [ht, Option.map_some, Option.some.injEq, Prod.mk.injEq] at h
    obtain ⟨rfl, rfl⟩ := h
    apply inv_update b r v o.key _ hv
    · obtain ⟨h1, h2, -, -⟩ := take_spec o.key oid b r o ht
      exact subLevel_at b r o.key o.sz _ (hv o.key) (by simpa using h1) (by simpa using h2)
    · intro k hk
      have hne : ¬ o.key = k := fun e => hk e.symm
      obtain ⟨h1, h2, -, -⟩ := take_spec k oid b r o ht
      simp only [hne, ite_false] at h1 h2; omega

private theorem cancel_inv.witness : ∃ (oid : Nat) (b b' : List Order) (v v' : View),
    Inv b v ∧ cancel oid b v = some (b', v') :=
  ⟨1, [⟨1, true, 5, 2⟩], _, update (fun _ => none) (true, 5) (some (2, 1)), _,
   by intro k; unfold update; by_cases e : k = (true, 5) <;> simp [e, cnt, tot, Order.key, eq_comm (a := (true, 5))],
   rfl⟩

theorem resize_inv (oid sz : Nat) (b b' : List Order) (v v' : View)
    (hv : Inv b v) (h : resize oid sz b v = some (b', v')) : Inv b' v' := by
  unfold resize at h
  by_cases hz : sz = 0
  · simp [hz] at h
  · simp only [hz, ite_false] at h
    cases ht : take oid b with
    | none => simp [ht] at h
    | some p =>
      obtain ⟨o, r⟩ := p
      simp only [ht, Option.map_some, Option.some.injEq, Prod.mk.injEq] at h
      obtain ⟨rfl, rfl⟩ := h
      apply inv_update b _ v o.key _ hv
      · obtain ⟨h1, h2, h3, h4⟩ := take_spec o.key oid b r o ht
        simp only [ite_true] at h1 h2 h3
        exact setLevel_at b _ o.key o.sz sz _ (hv o.key) (by omega) (by omega) (h3 sz) (h4 sz)
      · intro k hk
        have hne : ¬ o.key = k := fun e => hk e.symm
        obtain ⟨-, -, h3, h4⟩ := take_spec k oid b r o ht
        have := h3 sz
        simp only [hne, ite_false] at this
        exact ⟨by omega, h4 sz⟩

private theorem resize_inv.witness : ∃ (oid sz : Nat) (b b' : List Order) (v v' : View),
    Inv b v ∧ resize oid sz b v = some (b', v') :=
  ⟨1, 3, [⟨1, true, 5, 2⟩], _, update (fun _ => none) (true, 5) (some (2, 1)), _,
   by intro k; unfold update; by_cases e : k = (true, 5) <;> simp [e, cnt, tot, Order.key, eq_comm (a := (true, 5))],
   rfl⟩

theorem match1_inv (tbid : Bool) (tpx tsz : Nat) (b : List Order) (v : View)
    (m : Order) (b' : List Order) (v' : View)
    (hv : Inv b v) (h : match1 tbid tpx tsz b v = some (m, b', v')) : Inv b' v' := by
  unfold match1 at h
  by_cases hz : tsz = 0
  · simp [hz] at h
  · simp only [hz, ite_false] at h
    cases hp : pick tbid tpx b with
    | none => simp [hp] at h
    | some m0 =>
      simp only [hp, Option.bind_some] at h
      cases hf : fill m0 tsz b v with
      | none => simp [hf] at h
      | some q =>
        obtain ⟨b1, v1⟩ := q
        simp only [hf, Option.map_some, Option.some.injEq, Prod.mk.injEq] at h
        obtain ⟨-, rfl, rfl⟩ := h
        unfold fill at hf
        split at hf
        · exact cancel_inv _ _ _ _ _ hv hf
        · exact resize_inv _ _ _ _ _ _ hv hf

private theorem match1_inv.witness : ∃ (tbid : Bool) (tpx tsz : Nat) (b : List Order) (v : View)
    (m : Order) (b' : List Order) (v' : View),
    Inv b v ∧ match1 tbid tpx tsz b v = some (m, b', v') :=
  ⟨true, 5, 1, [⟨1, false, 5, 2⟩], update (fun _ => none) (false, 5) (some (2, 1)), _, _, _,
   by intro k; unfold update; by_cases e : k = (false, 5) <;> simp [e, cnt, tot, Order.key, eq_comm (a := (false, 5))],
   rfl⟩

/-! ## Price-time priority -/

private theorem best_none (bid : Bool) (b : List Order) (h : best bid b = none) :
    ∀ o ∈ b, o.bid ≠ bid := by
  induction b with
  | nil => simp
  | cons x xs ih =>
    unfold best at h
    cases hb : best bid xs with
    | none =>
      simp only [hb] at h
      by_cases hx : x.bid = bid
      · simp [hx] at h
      · intro o ho; cases ho with
        | head => exact hx
        | tail _ hm => exact ih hb o hm
    | some q =>
      simp only [hb] at h; split at h <;> cases h

private theorem best_le (bid : Bool) (b : List Order) (p : Nat) (h : best bid b = some p) :
    ∀ o ∈ b, o.bid = bid → better bid o.px p = false := by
  induction b generalizing p with
  | nil => simp
  | cons x xs ih =>
    intro o ho hob
    unfold best at h
    cases hb : best bid xs with
    | none =>
      simp only [hb] at h
      by_cases hx : x.bid = bid
      · simp only [hx, ite_true, Option.some.injEq] at h; subst h
        cases ho with
        | head => cases bid <;> simp [better]
        | tail _ hm => exact absurd hob (best_none bid xs hb o hm)
      · simp [hx] at h
    | some q =>
      simp only [hb] at h
      have ihq := ih q hb
      by_cases hc : x.bid = bid ∧ better bid x.px q = true
      · simp only [hc, and_self, ite_true, Option.some.injEq] at h; subst h
        cases ho with
        | head => cases bid <;> simp [better]
        | tail _ hm =>
          have := ihq o hm hob
          cases bid <;> simp [better] at this hc ⊢ <;> omega
      · simp only [hc, ite_false, Option.some.injEq] at h; subst h
        cases ho with
        | head =>
          have : ¬ better bid x.px q = true := fun e => hc ⟨hob, e⟩
          simpa using this
        | tail _ hm => exact ihq o hm hob

private theorem find_prefix (pr : Order → Bool) (b : List Order) (m : Order)
    (h : b.find? pr = some m) : ∃ pre post, b = pre ++ m :: post ∧ ∀ o ∈ pre, pr o = false := by
  induction b with
  | nil => simp at h
  | cons x xs ih =>
    by_cases hx : pr x = true
    · simp only [List.find?_cons, hx] at h
      cases h; exact ⟨[], xs, by simp, by simp⟩
    · simp only [List.find?_cons, hx] at h
      obtain ⟨pre, post, he, hp⟩ := ih h
      refine ⟨x :: pre, post, by simp [he], ?_⟩
      intro o ho
      cases ho with
      | head => simpa using hx
      | tail _ ho => exact hp o ho

/-- The maker of a fill is an opposite-side order at the best opposite price
(nothing on that side is better) and every order ahead of it in the queue
has another side or price (time priority). -/
theorem match1_priority (tbid : Bool) (tpx tsz : Nat) (b : List Order) (v : View)
    (m : Order) (b' : List Order) (v' : View)
    (h : match1 tbid tpx tsz b v = some (m, b', v')) :
    m ∈ b ∧ m.bid = !tbid ∧
    (∀ o ∈ b, o.bid = !tbid → if tbid then m.px ≤ o.px else o.px ≤ m.px) ∧
    ∃ pre post, b = pre ++ m :: post ∧ ∀ o ∈ pre, ¬ (o.bid = m.bid ∧ o.px = m.px) := by
  unfold match1 at h
  by_cases hz : tsz = 0
  · simp [hz] at h
  · simp only [hz, ite_false] at h
    cases hp : pick tbid tpx b with
    | none => simp [hp] at h
    | some m0 =>
      simp only [hp, Option.bind_some] at h
      cases hf : fill m0 tsz b v with
      | none => simp [hf] at h
      | some q =>
        simp only [hf, Option.map_some, Option.some.injEq, Prod.mk.injEq] at h
        obtain ⟨rfl, -, -⟩ := h
        unfold pick at hp
        cases hb : best (!tbid) b with
        | none => simp [hb] at hp
        | some p =>
          simp only [hb, Option.bind_some] at hp
          split at hp
          · have hmem := List.mem_of_find?_eq_some hp
            have hsat := List.find?_some hp
            simp only [decide_eq_true_eq] at hsat
            obtain ⟨pre, post, he, hpre⟩ := find_prefix _ b m0 hp
            refine ⟨hmem, hsat.1, ?_, pre, post, he, ?_⟩
            · intro o ho hob
              have := best_le _ b p hb o ho hob
              rw [hsat.2]; cases tbid <;> simp [better] at this ⊢ <;> omega
            · intro o ho ho2
              have := hpre o ho
              simp only [decide_eq_false_iff_not] at this
              exact this ⟨ho2.1.trans hsat.1, ho2.2.trans hsat.2⟩
          · cases hp

private theorem match1_priority.witness : ∃ (tbid : Bool) (tpx tsz : Nat) (b : List Order) (v : View)
    (m : Order) (b' : List Order) (v' : View),
    match1 tbid tpx tsz b v = some (m, b', v') :=
  ⟨true, 5, 1, [⟨1, false, 5, 2⟩], update (fun _ => none) (false, 5) (some (2, 1)), _, _, _, rfl⟩

/-! ## Bounded checks and differential vectors -/

def keys : List Key := [(true, 4), (true, 5), (false, 5), (false, 6)]

def invB (b : List Order) (v : View) : Bool :=
  keys.all fun k => v k == if cnt k b = 0 then none else some (tot k b, cnt k b)

inductive Op where
  | rest (o : Order) | restAt (i : Nat) (o : Order) | cancel (oid : Nat)
  | resize (oid sz : Nat) | take1 (bid : Bool) (px sz : Nat)
deriving Repr

def step (s : List Order × View) : Op → List Order × View
  | .rest o => (rest o s.1 s.2).getD s
  | .restAt i o => (restAt (min i s.1.length) o s.1 s.2).getD s
  | .cancel oid => (cancel oid s.1 s.2).getD s
  | .resize oid sz => (resize oid sz s.1 s.2).getD s
  | .take1 bid px sz => ((match1 bid px sz s.1 s.2).map fun r => r.2).getD s

def ops : List Op :=
  [.rest ⟨1, true, 5, 2⟩, .rest ⟨2, true, 5, 3⟩, .rest ⟨3, false, 6, 1⟩, .rest ⟨4, true, 4, 0⟩,
   .restAt 0 ⟨5, true, 5, 4⟩, .cancel 1, .cancel 9, .resize 2 7, .resize 3 0,
   .take1 false 5 3, .take1 true 6 1, .take1 false 4 9]

/-- Every sequence of up to `n` ops from `ops`, from the empty book: the
states where `invB` fails (must be none). -/
def badRuns : Nat → List (List Order × View) → List (List Order × View)
  | 0, ss => ss.filter fun s => !invB s.1 s.2
  | n + 1, ss => badRuns n (ss ++ ss.flatMap fun s => ops.map (step s))

def init : List Order × View := ([], fun _ => none)

#guard (badRuns 3 [init]).isEmpty

-- Reach: a fill that empties a level and one that leaves a partial maker both occur.
#guard (let s := step (step init (.rest ⟨1, false, 5, 2⟩)) (.take1 true 5 2); s.1 = [] ∧ s.2 (false, 5) = none)
#guard (let s := step (step init (.rest ⟨1, false, 5, 2⟩)) (.take1 true 5 1); s.2 (false, 5) = some (1, 1))

/-- Differential vectors for `crates/hl-exec/tests/lean_vectors.rs`: after
each op sequence, the L2 at the four keys as `side px total count`. -/
def vectors : List (List Op) :=
  [[.rest ⟨1, true, 5, 2⟩, .rest ⟨2, true, 5, 3⟩, .cancel 1],
   [.rest ⟨1, false, 6, 2⟩, .rest ⟨2, false, 6, 3⟩, .take1 true 6 4],
   [.rest ⟨1, true, 5, 2⟩, .restAt 0 ⟨2, true, 5, 3⟩, .resize 1 9, .take1 false 5 4],
   [.rest ⟨1, false, 5, 2⟩, .rest ⟨2, false, 6, 3⟩, .take1 true 6 4],
   [.rest ⟨1, true, 4, 2⟩, .rest ⟨2, true, 5, 3⟩, .take1 false 4 10]]

#eval do
  for i in List.range vectors.length do
    let s := (vectors.getD i []).foldl step init
    let l2 := keys.map fun k => match s.2 k with
      | none => s!"{k.1} {k.2} 0 0"
      | some (t, c) => s!"{k.1} {k.2} {t} {c}"
    IO.println s!"VECTOR {i}: {String.intercalate "; " l2} | queue {s.1.map Order.oid}"

end HlBook
