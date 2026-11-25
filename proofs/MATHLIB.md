# 用不用 Mathlib：一条权衡，不是一个立场

先纠正一处措辞。之前写「不依赖 Mathlib」像是在夸自己，这是错的 —— **那是限制**，
不是优点。Mathlib 已经在本机编好（见下），该用的时候就用。

## 事实（2026-11-24 实测）

| 项 | 数值 |
|---|---|
| Mathlib 从源码全编 | 约 2.5 小时（32 并发） |
| 磁盘 | 13.26 GB |
| 空跑 `lake build`（未改动） | 14.4 秒 |
| **`import Mathlib` 的单文件检查** | **125 秒** |
| 窄 import（`Mathlib.Analysis.SpecialFunctions.Exp`） | 17 秒 |

## 用它能换来什么：AIMD 那条从 12 行变 5 行

同一道题，两种写法。

**核心库版**（`Proofs/Aimd.lean`，需要手动搬分配律 + `omega` 收尾）：

```lean
theorem climb_closed (m : Nat) : 2 * climb m = 3 * m * (m + 1) := by
  induction m with
  | zero => decide
  | succ m ih =>
      simp only [climb]
      rw [Nat.mul_add, ih]
      simp only [Nat.mul_add, Nat.add_mul, Nat.mul_comm, Nat.mul_left_comm]
      omega
```

**Mathlib 版**（本机实测通过，`lake env lean McdAimd.lean` 退出码 0）：

```lean
theorem climb_closed (m : Nat) : 2 * climb m = 3 * m * (m + 1) := by
  induction m with
  | zero => simp [climb]
  | succ m ih =>
      simp only [climb]
      rw [Nat.mul_add, ih]
      ring
```

`ring` 一次收尾，不用猜该搬哪条分配律。**这就是那 2.5 小时买来的东西。**

## 为什么不把它设成 `proofs/` 的依赖

`proofs/` 的定位是「克隆下来十秒能验」。一旦 `lakefile.toml` 里写上 `require mathlib`：

- 克隆者要下 + 编约 2.5 小时（Windows 上 `lake exe cache get` 还是坏的，见 `math.md`）；
- 任何 `import Mathlib` 的文件，每次检查两分钟起。

所以按**文件**决定，而不是按**项目**决定：

| 场景 | 放哪 | 检查方式 |
|---|---|---|
| 纯算术不变量（片切分、退避上界、对齐） | `proofs/Proofs/*.lean`，只用核心库 | `lake build`，十秒 |
| 需要 `ring` / 分析学 / 概率论的 | 单独的文件，`import Mathlib` | 在已编好 Mathlib 的工程里 `lake env lean <file>`，两分钟 |

## 已经用上的一次

`mc-downloader/proofs/Proofs/Aimd.lean` 的 `climb_closed` 在 Mathlib 工程里被**同一句话**证掉了（`ring`）。
它证明的是同一件事，只是不再需要 12 行洗牌。

## 已经用 Mathlib 证掉的一条：停滞检测的尾概率

`proofs/mathlib/Stall.lean`（本仓库里的参考副本，**不由** `proofs/lakefile.toml` 构建）：

```lean
theorem expMeasure_Ioi (r x : ℝ) (hr : 0 < r) (hx : 0 ≤ x) :
    expMeasure r (Set.Ioi x) = ENNReal.ofReal (Real.exp (-(r * x)))
```

证明路径：`ofReal_cdf`（`ENNReal.ofReal (cdf μ x) = μ (Iic x)`）→ `cdf_expMeasure_eq`
→ `measure_compl` 取补 → `ENNReal.ofReal_sub` 收尾。本机实测 `lake env lean Stall.lean` **exit 0**。

**三路一致**（同一个数，三个引擎）：

| 来源 | 结果 |
|---|---|
| Lean 定理 | `exp (-(r*x))`，对任意 `r>0`、`x≥0` |
| Wolfram `Integrate` | `E^(-(lam*w))` |
| R `pexp(10, 0.25, lower.tail = FALSE)` | **0.082085** |

**这条证明的假设**：相邻进展的间隔服从 `expMeasure r`（指数分布）。
它**没有**证“进展是泊松过程” —— Mathlib 里没有点过程，那是建模选择，不是定理。

## 检查命令

```powershell
cd E:\Users\lmq\lean-math\mathlib-probe
lake env lean Stall.lean      # 约 2 分钟（import Mathlib 冷启动）
lake env lean McdAimd.lean    # ring 版 AIMD
```
