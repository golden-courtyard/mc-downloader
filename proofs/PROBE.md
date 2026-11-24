# proofs/PROBE.md —— 核心库可用性基线（2026-11-24 实测）

> 为什么要有这份文件：Lean 的核心库与 Mathlib 边界不清，凭"我记得它能用"写证明会一头撞墙。
> 本文件是**测出来的**清单；换引擎版本后先重跑下面的探针，再动手。

## 探针

```lean
-- 一条命令跑完：lean probe.lean
#check Rat
#check Int
#check Nat.pow
#check List.sum
#check List.range
#check List.range_succ
#check List.sum_append
#check List.sum_cons
#check List.map_cons
#check Nat.mod_lt
#check Nat.pow_succ
#check Nat.mul_add
#check Nat.add_mul
#check Nat.mul_assoc
#check Nat.le.intro
```

## 结果

| 项 | 结论 | 备忘 |
|---|---|---|
| Lean 版本 | **4.36.0**（elan，`x86_64-w64-windows-gnu`） | `lean-toolchain` 固定同版本 |
| `Rat` / `Int` / `Nat.pow` | 可用 | 有理数在核心里，不用 Mathlib |
| `List.sum` / `List.range` / `range_succ` / `sum_append` / `sum_cons` / `map_cons` | 可用 | 求和与列表拆分的证明靠它们 |
| `Nat.mod_lt` / `Nat.mul_add` / `Nat.add_mul` / `Nat.mul_assoc` / `Nat.pow_succ` / `Nat.le.intro` | 可用 | 抖动上界与对齐证明靠它们 |
| `omega` / `simp` / `rw` / `induction` / `obtain` / `decide` | 可用 | 本轮全部证明只用到这些 |
| `Finset` | **无**（Mathlib） | 所以求和一律用 `List.sum`，不用 `Finset.sum` |
| `Nat.dvd_mul_of_dvd_left` | **无**（Mathlib） | 对齐用"存在商"表述，不用 `∣` 的引理 |
| `Nat.ModEq` | **无** | 同上 |
| `ring` / `norm_num` / `nlinarith` / `field_simp` | **无**（Mathlib） | 多项式整理只能靠 `Nat.mul_add` 一类重写 + `omega` |
| Mathlib | **在编**（2026-11-24 17:56 起 `lake` + 约 30 个 `lean` 进程） | 本轮不依赖它 |

## 为什么不进 CI

- 本目录总量不足 200 行，且只依赖核心库，本地 `pwsh scripts/check-lean.ps1` 一秒级完成；
- 进 CI 要装 elan、要缓存 toolchain，等于给发布链加一个新的失败面（CodeQL 那轮已经吃过"先推红再修"的亏）；
- **再评估条件**：Mathlib 编译完成、且确需形式化需要分析学的结论（例如停滞检测的 `exp(-λW)`）时，再单独决定。

## 已知的证明坑（写下来省下次的时间）

1. `2 * climb m` 不是子项时，`rw [ih]` 会失败 —— 先 `rw [Nat.mul_add]` 把它拆出来；
2. `omega` 把 `m * (m + 1)` 当原子，**跨不相等形式**的两个多项式它不会替你展开 ——
   需要先用 `calc` 把两边化成同一个原子，或补一条 `Nat.mul_comm` 之类的重写；
3. `simp only [...]` 里多写一条用不上的引理会给 warning（本仓库当错误处理）—— 留最少的几条。
