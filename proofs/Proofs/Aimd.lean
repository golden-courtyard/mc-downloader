/-- 一轮锯齿的并发和：峰值 `2m`，砍半到 `m` 后每 tick +1，共 `m + 1` 个 tick。

递推的由来：多一个 tick 时，已有 `m + 1` 项各 +1，再加新项 `2m + 2`，合计 `+3m + 3`。

对应实现：`src/download.rs` 的 AIMD（每 tick +1、掉幅 > 25% 时砍半）。
假设：理想化模型 —— 无噪声、每轮都能长回峰值、砍半后立即恢复。
未证明：真实网络下的锯齿形状（噪声会让周期与幅度偏离）。 -/
def climb : Nat → Nat
  | 0 => 0
  | m + 1 => climb m + (3 * m + 3)

/-- 关键恒等式：`2 × 并发和 = 3·m·(m+1)`。

有了它，时间平均 = `climb m / (m+1)` = `3m/2`，即峰值 `2m` 的 75%。 -/
theorem climb_closed (m : Nat) : 2 * climb m = 3 * m * (m + 1) := by
  induction m with
  | zero => decide
  | succ m ih =>
      simp only [climb]
      rw [Nat.mul_add, ih]
      simp only [Nat.mul_add, Nat.add_mul, Nat.mul_comm, Nat.mul_left_comm]
      omega

/-- 同一件事换个说法（不引入除法）：`4 × 并发和 = 3 × 峰值 × tick 数`，
即「平均并发 = 0.75 × 峰值」。 -/
theorem avg_is_three_quarters (m : Nat) : 4 * climb m = 3 * (2 * m) * (m + 1) := by
  have h := climb_closed m
  calc 4 * climb m = 2 * (2 * climb m) := by
        rw [show (4 : Nat) = 2 * 2 from rfl, Nat.mul_assoc]
    _ = 2 * (3 * m * (m + 1)) := by rw [h]
    _ = 3 * (2 * m) * (m + 1) := by
          simp only [Nat.mul_assoc, Nat.mul_comm, Nat.mul_left_comm]
