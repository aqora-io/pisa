theorem a (p q r : Prop) (hp : p) (hq : q) (hr : r) : (p ∧ q) ∧ (q ∧ r) := by
  constructor
  · constructor
    · exact hp
    · exact hq
  · constructor <;> assumption

theorem b (n : Nat) : n + 0 = n ∧ 0 + n = n := by
  refine ⟨?_, ?_⟩
  all_goals first | rfl | simp

theorem c (xs : List Nat) : xs ++ [] = xs := by
  induction xs with
  | nil => rfl
  | cons x xs ih =>
    simp

theorem d (a b : Nat) (h : a = b) : b = a := by
  have h2 : b = a := h.symm
  exact h3
