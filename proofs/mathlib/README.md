# 需要 Mathlib 的证明（参考副本）

这里的 `.lean` 文件**不由** `proofs/lakefile.toml` 构建 —— 它们需要 Mathlib（本机 13.26 GB、全编 2.5 小时）。
放在这里的目的是：让克隆仓库的人能看到证明本身，而不必先下载 13 GB。

## 怎么检查

```powershell
cd <一个已 lake build 过 Mathlib 的工程>
# 把本目录的文件拷进去（或加 --root），然后：
lake env lean McdStall.lean                # 约 2 分钟（import Mathlib 的冷启动）
lake env lean McdAimd.lean
```

## 有什么

| 文件 | 内容 | 为什么需要 Mathlib |
|---|---|---|
| `Stall.lean` | `expMeasure r (Ioi x) = ofReal (exp (-(r*x)))` —— 停滞检测的尾概率 | 指数分布、CDF、ENNReal 都在 Mathlib |
| `Aimd.lean` | 与 `Proofs/Aimd.lean` 同题，`ring` 收尾（5 行 vs 12 行） | `ring` 在 Mathlib |

## 与 `proofs/` 的分工

见 [`../MATHLIB.md`](../MATHLIB.md)：纯算术留在 `proofs/`（十秒可验），需要分析学的单列在这里（两分钟/文件）。
