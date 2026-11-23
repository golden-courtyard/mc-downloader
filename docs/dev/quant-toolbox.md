# 定量分析工具箱（R 为主）

配套 [`scripts/quant/README.md`](../../scripts/quant/README.md) 的分工表：Python 取数 → **R 统计与图** → Wolfram 交叉验算。
这篇回答两个问题：**R 的离线文档怎么用**、**我们这个下载器还有哪些 R 能力没用上**。

## 零、Wolfram 不是配角（修订）

我第一版把 Wolfram 写成"关键数字交叉验算"，那是低估。参照项目 `bid-info` 的做法说得更准
（`analysis/bid_info_cybernetics.wl` 文件头）：

> 与 `bid_info_cybernetics.R` 同源同口径，但把「统计代理」换成**真的控制论对象**：
> 状态空间 / 传递函数 / 极点 / 稳定性 / 噪声增益。**R 那边给数；这边给解析式。**

正确的分工是：

| 谁 | 给什么 | 不能给什么 |
|---|---|---|
| **R** | 估计、区间、检验、图（数据侧） | 解析式；符号化简；任意精度 |
| **Wolfram** | **解析对象**（传递函数/极点/稳定性/噪声增益）、符号核验、精确与任意精度算术、解析优化 | 大规模数据清洗；分位回归这类统计推断（见下） |

`bid-info` 还有两条值得抄的做法：

- **负结果也写下来**：`bid_info_qreg.wl` 明说"分位回归在 15.0 上语法未验证，用 R 的 `quantreg` 结果"——
  工具试过、不行、写在文件头，比悄悄不用强。
- **符号核验是独立交付**：他们有一次提交是"Wolfram 恢复后补做符号核验，并撤销此前『只能数值』的局限标注"。
  也就是说，某些结论**只有符号推导能证**，数值算得再一致也不算证过。

## 一、离线文档（不联网也能查）

`help()` 在 `Rscript` 里会往 pager 走，非交互下不可靠。稳的写法：

```r
# 读某个函数的 Rd 原文
db <- tools::Rd_db("stats")
tools::Rd2txt(db[["median.Rd"]]) |> writeLines()

# 包的说明 / 版本 / 依赖
utils::packageDescription("quantreg")[c("Title", "Version", "Description")]

# 已安装包的 vignette 清单（真正的教程都在这儿）
vignette(package = "survival")$results[, c("Item", "Title")]
```

本机已装的这几个包，vignette 就是最好的离线教材：`boot`（bootstrap 的三种区间）、
`survival`（删失与 Cox）、`quantreg`（分位回归）、`lme4`（混合效应）、`fitdistrplus`（分布拟合与 GOF）。

R 本身的新写法也值得用：原生管道 `|>`、匿名函数 `\(x) x + 1`、`dplyr` 的 `.by =` 分组，
以及 `vctrs` 的严格类型 —— 我们 CSV 里 `impl_per_100_commits` 空串那类问题，用 `readr::type_convert`
或 `vctrs::vec_cast` 会当场报错而不是悄悄变字符。

## 二、问题 → 工具（我们真会问的那些）

| 我们会问的问题 | 包 / 能力 | 为什么是它 | 本机 |
|---|---|---|---|
| 吞吐随并发/片大小怎么变，**带不确定性** | `lme4`（run 随机截距）、`sandwich` + `lmtest`（HC3）、`boot`（整段重抽） | 同一轮里多个分片不独立；只看均值会把"波动"当成"差异" | 已装 |
| **尾部分位**（p99 决定超时与 `StallGuard` 窗口） | `quantreg` | 分位回归直接对 p99 建模；均值对尾部不敏感 | 已装 |
| 超时/stall 判定准不准 | `survival`（KM、Cox） | 观测是**删失**的：没 stall 的区间并不知道它以后会不会 stall，比例/均值都不成立 | 已装 |
| 延迟与失败时长的分布形状 | `fitdistrplus` | 拟合 + GOF（对数正态 vs 威布尔），用来解释"为什么 p99 比 p50 差 10 倍" | 已装 |
| 参数选择的实验设计（conns × timeout × 片大小 × 限速） | `DoE.base` / `FrF2`（因子设计）、`rsm`（响应面） | one-at-a-time 要 N 倍实验才能看到交互；因子设计一次给主效应+交互 | 待装 |
| **要跑几次才能看出差异** | `pwr` | 我们常只有 3–5 次跑；先算功效，再决定值不值得测 | 待装 |
| 阶段漂移（换 TLS 后端 / 换 CI runner 后有没有变） | `changepoint` / `strucchange` | 人眼看"好像变快了"不可靠；这两个能给出变点与置信度 | 待装 |
| 组间对比与效应量（官方 vs 镜像；alpha vs rc） | `emmeans`、`effectsize`、`coin`（精确非参） | 对比要带 CI；小样本用精确检验而不是近似 | 待装 |
| 大 CSV（百万级分片日志） | `data.table::fread`、`duckdb`（直接在 CSV 上 SQL）、`arrow`（parquet） | 现在用 `read.csv` 会慢到不想重跑 | 待装 |
| 拼图 | `patchwork` | 比 `gridExtra` 好用；我们现在的 `if (gridExtra_ok)` 是个将就 | 待装 |
| 分析可复现 | `renv`（锁包版本）、`targets`（依赖图，只重跑下游） | 报告里的数要能一年后复现；R 包版本漂移是隐形杀手 | 待装 |
| 表格 | `gt` / `kableExtra` | 报告里的表目前手写 Markdown；数一改就得人肉同步 | 待装 |

## 二·五、我们自己的三个解析对象（已落地）

脚本：`scripts/quant/analysis/downloader_analytics.wls`（Wolfram），常量取自语料库源码
（`src/source.rs` 的 `ALPHA = 0.3`；`src/download.rs` 的 AIMD 规则与 4096 字节进展下限）。

| 解析对象 | 结果 | 意义 |
|---|---|---|
| `SourcePool` 的 EWMA：`H(z) = a/(1-(1-a)z^-1)` | 方差增益 `a/(2-a)` = **0.176**（标准差保留 42%）；半衰期 **1.94** 个样本；极点 `1-a = 0.7` | α=0.3 的选择有数可依：既要灵敏（半衰期约 2 次测量），又要压噪（方差只剩 17.6%） |
| AIMD 锯齿（+1/tick，掉幅 >25% 砍半） | 平均并发恒为峰值的 **75%**，与峰值 B 无关 | 若吞吐 ∝ 并发，**锯齿本身就是 25% 的吞吐代价**；这条以前只有"会有锯齿"的定性说法 |
| `StallGuard` 误判概率（Poisson 进展，每 4096 B 一次） | `P(无进展 > W) = Exp[-λW]`：1 KB/s 时 10 s 窗口误判 **8.2%**；4 KB/s → 4.5e-5；64 KB/s → 3.3e-70 | 窗口取 `max(10s, 2×timeout)` 而不是固定 5 s 的定量理由 |

这就是"R 给数、Wolfram 给式"在我们仓库里的样子：三条都不是"再算一遍同一个数"，
而是**得到数值方法给不出的结论**（75% 这个常数、指数衰减律）。

## 三、现在就能做的三件（只用已装的包）

数据都在 `scripts/quant/raw/`：

1. **`e1-kali.csv`（43 次吞吐）** —— 目前报告只给均值/排名。改用 `lm(log(mbps) ~ conns + timeout + retry, ...)`
   + `sandwich` 的 HC3 稳健标准误，再用 `boot` 做整段重抽的 CI，回答"并发提到 16 到底还值不值"。
2. **`piece-times.csv`（64 行 burst × 并发）** —— 用 `quantreg` 把 `p99_secs` 当响应，
   看**尾部分位**随并发怎么变（均值可能平，尾部可能在恶化）。
3. **`e8-lock.csv`（26 次写尝试）** —— `attempt` 是离散时间："第几次尝试成功"用 `survival`
   的离散时间模型（或 KM）比"成功率 96%"信息量大得多；失败率用 `boot` 给区间。

三件的产物都是"图 + 一个可复现脚本"，不是"再写一篇报告"。

## 四、建议先装的五个

```r
install.packages(c("patchwork", "renv", "data.table", "pwr", "duckdb"))
# 用的时候：duckdb 直接 read_csv_auto('scripts/quant/raw/*.csv') 跑 SQL，不建库
```

CRAN 本机可达（2026-11-23 实测 `cloud.r-project.org` HEAD 200）。

**不建议**：`tidyverse` 全家桶（体积与收益不符，我们要的 `dplyr`/`ggplot2` 已经在）、
`rmarkdown`（成品是 `docs/reports/*.md`，溯源靠脚本而不是渲染器）、`rstan`（贝叶斯在这个样本量下不划算）。

## 五、与参照项目的差距

`Academic/research/bid-info` 用了 `lme`（混合效应）、`qreg`（分位回归）、时间序列、离散事件模拟（DES）；
我们目前只用到**描述统计 + 一张图**。差距不在工具，在**问题还没被问到那一步**：
"并发提到 16 还值不值""超时设 10s 够不够"这类问题，本质是**带不确定性的比较**与**尾部建模**，
正好是上表前两行。

## 六、不用 MATLAB

本机那份是盗版，不放进交付链路。数值与统计用 R，符号推导用 Wolfram；
需要"两个引擎对同一批数各算一遍"时，照 `versioning_vs_notes.R` / `.wls` 那对文件的样子写。
