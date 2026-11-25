-- 在 `mathlib-probe` 工程里检查（需要 Mathlib；本目录的文件**不**由 proofs/lakefile 构建）。
-- 与 `proofs/Proofs/Aimd.lean` 证的是同一件事，只是用 `ring` 收尾：
-- 核心库版要手动搬分配律 + omega，这里 5 行。
import Mathlib

def climb : Nat → Nat
  | 0 => 0
  | m + 1 => climb m + (3 * m + 3)

theorem climb_closed_ring (m : Nat) : 2 * climb m = 3 * m * (m + 1) := by
  induction m with
  | zero => simp [climb]
  | succ m ih =>
      simp only [climb]
      rw [Nat.mul_add, ih]
      ring
