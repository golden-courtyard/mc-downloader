import Mathlib

set_option linter.deprecated false

open MeasureTheory ProbabilityTheory

/-- 停滞检测的尾概率：相邻进展间隔服从 `expMeasure r` 时，
窗口 `x` 内没有进展的概率是 `exp (-(r*x))`。

数值对照（`docs/reports/` 里那张表）：`r = 1024/4096 = 0.25`、`x = 10` ⇒ `exp (-2.5) = 0.082085`，
与 R 的 `pexp(10, 0.25, lower.tail = FALSE)` 逐位一致。

**本文件不由 `proofs/lakefile.toml` 构建** —— 它在已编好 Mathlib 的工程里检查：
`cd E:\Users\lmq\lean-math\mathlib-probe && lake env lean Stall.lean`（约 2 分钟）。 -/
theorem expMeasure_Ioi (r x : ℝ) (hr : 0 < r) (hx : 0 ≤ x) :
    expMeasure r (Set.Ioi x) = ENNReal.ofReal (Real.exp (-(r * x))) := by
  haveI := isProbabilityMeasure_expMeasure hr
  have hcdf : expMeasure r (Set.Iic x) = ENNReal.ofReal (1 - Real.exp (-(r * x))) := by
    rw [← ofReal_cdf (expMeasure r) x, cdf_expMeasure_eq hr x, if_pos hx]
  rw [← Set.compl_Iic, measure_compl measurableSet_Iic (measure_ne_top _ _), measure_univ, hcdf]
  have hexp_le : Real.exp (-(r * x)) ≤ 1 := by
    rw [Real.exp_le_one_iff]
    nlinarith
  have hnonneg : 0 ≤ 1 - Real.exp (-(r * x)) := by linarith
  rw [← ENNReal.ofReal_one, ← ENNReal.ofReal_sub 1 hnonneg]
  congr 1
  ring
