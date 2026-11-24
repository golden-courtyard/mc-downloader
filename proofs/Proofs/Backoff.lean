/-- 抖动落在 `[0, 500)`。

对应实现：`src/download.rs` 的 `backoff_delay` 用 `seed % 500` 当抖动。
假设：`seed` 任意自然数。
未证明：抖动确实能把「多片同刻重试」打散 —— 那是机制论证，不是定理。 -/
theorem jitter_lt (seed : Nat) : seed % 500 < 500 := Nat.mod_lt seed (by decide)

/-- 退避时长 = `2^a + jitter`，因此严格落在 `[2^a, 2^a + 500)`。 -/
theorem backoff_bounds (a seed : Nat) :
    2 ^ a ≤ 2 ^ a + seed % 500 ∧ 2 ^ a + seed % 500 < 2 ^ a + 500 := by
  refine ⟨Nat.le.intro rfl, ?_⟩
  exact Nat.add_lt_add_left (jitter_lt seed) _
