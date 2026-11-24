/-- 4 KiB 对齐：片大小是 4096 的倍数时，任意片数下每个边界仍是 4096 的倍数。

对应实现：`src/download.rs` 按 `size / conns` 切片，并把片边界对 4096 取整。
假设：片大小 = 4096 × 整数；片数任意。
未证明：对齐带来的性能收益 —— 那是 `docs/reports/quant-report-20261122.md` 的实测，不是定理。 -/
theorem aligned_mul (n k : Nat) (h : ∃ q, n = 4096 * q) : ∃ q', n * k = 4096 * q' := by
  obtain ⟨q, hq⟩ := h
  exact ⟨q * k, by rw [hq, Nat.mul_assoc]⟩
