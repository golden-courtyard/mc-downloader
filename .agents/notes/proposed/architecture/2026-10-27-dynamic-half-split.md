# Agent Note: 动态分段 —— 从「一次算死片大小」到「边下边对半切」

Status: proposed — 当前 `piece.rs` 的 `with_conns` 只在**下载开始前**算一次片大小，下载过程中不再变；IDM 的做法是**边下边切**

## Problem

`with_conns` 只在开跑前算一次片大小，运行中不再变：先来的慢连接可能领走一个巨大的片，后面的连接没活干，
整批的连接利用率被**队头大片**拖住。IDM 的公开做法是边下边切（新连接可用时把最大段对半分），本提案要的就是这一步。
成立条件被 2026-11-22 的数据收窄过，见本文末「数据给这条提案划了边界」一节。

## Proposal

### 规则来源：IDM 官方文档（不是逆向）

`internetdownloadmanager.com/support/segmentation.html` 公开写了它的算法，标题就叫
**"Dynamic file segmentation that uses in-half division rule"**。四条规则：

| # | 官方原文 | 含义 |
|---|---|---|
| 1 | *"When new connection becomes available IDM finds the largest segment to download and divide it in half"* | 新连接可用 ⇒ **找最大段、对半分** |
| 2 | *"Once a connection has downloaded a segment … if the next connection has not started … IDM reassignes the segment to the first connection"* | 下完一段、下一连接还没开工 ⇒ **那段重新给它** |
| 3 | *"If the next connection has started … first connection helps other slowly working connections by dividing the largest segment in half"* | 下一连接已开工 ⇒ **帮忙切最大段** |
| 4 | *"IDM won't divide the segment only when its size is too small for this connection type"* | **段太小就不切** |

**⇒ 关键差别不是"切成几片"，是"什么时候切"**：
我们**下载前**算好片大小（静态），IDM **下载过程中**持续把最大段对半（动态）。

### 我们已有的一半地基

`PieceScheduler` 的设计注释写着：

> 设计参照 continuous batching（Orca, Yu et al. 2022；vLLM） —— **不等当前批完成，新任务随时插入**

`push` / `push_many` 本来就是为"运行中插入"准备的 —— **地基有，只是切片策略还没用上它。**

### Proposal：在取片时做对半

不改调度器结构，只改**取片的那一步**（`scheduler.rs::next` 的调用方）：

```
worker 取片时：
  若队列中长期存在一个「比最小可切阈值大 2 倍以上」的片：
    把它对半切成 A + B，自己取 A，把 B 放回队列（push）
```

**判据（对半的条件）**：
- 该片大小 `> 2 × MIN_PIECE`（否则切完两半都小于下限 —— 对应官方规则 4）
- **且队列里没有更小的片可领**（否则没必要切，直接领小的）

**注意**：对半切的是**尚未开始的片**，不是"把正在下的片打断"。
正在传输的 HTTP Range 请求无法中途改范围（HTTP/1.1 的 Range 是请求时定死的），
真要实现"抢正在下的段"需要 HTTP/2 的 **改 byte range** 能力（MP-H2 论文用过这个技巧），
**本项目不做** —— 那是另一个量级的改动。

### 与其他决策的关系

- **`with_conns`（`afb7226`）解决的是"片数够不够"** —— 让每个连接有活干
- **本篇解决的是"干得均不均"** —— 让慢连接不要被分到大片拖尾
- **两者不冲突，是互补的**：先保证有片（`with_conns`），再保证公平（对半切）

### 2026-11-22 补：数据给这条提案划了边界（未推翻，但收窄）

`../../../docs/reports/quant-report-20261122.md` 的 E1/E2：

- **环路不限速**：吞吐峰值在 `--piece-conns 4`，之后单调下降（64 时只剩峰值的 36%）⇒ 这类链路上"边下边切"只会更差；
- **带宽受限（netem 400 mbit）**：1→64 连接全平（43.5–46.1 MB/s）⇒ 切不切没差别；
- **目标盘是机械盘**：连接越多墙钟越慢（4 比 1 慢 1.60×、16 慢 2.06×）⇒ 动态加并行是负优化。

⇒ **成立条件被收窄到一种**：源**按连接限速**（如 GitHub 92 KB/s、单连接 0.75–1 MB/s 的镜像），
且目标盘不是机械盘 —— 那正是 `docs/user/usage.md` 里 `--piece-conns` 一节的场景。
所以它**不该**成为默认行为，在 {带宽受限 / 机械盘} 两类链路上都不该触发；
真要实现，判据得先写清"何时允许切"。写路径的正事见
[write-locality](2026-11-22-write-locality.md) 与 issue #8。

## Alternatives considered

| 方案 | 为什么不做 |
|---|---|
| **保持现状（静态切片）** | **够用但不是最优**：片偏大时，接手大片的慢连接会拖尾。IDM 的"对半"就是为了消除这个拖尾 |
| **按实测速度动态算片大小**（第三方逆向分析提到的 `剩余大小/(剩余线程数×速度衰减因子)`）| **与"对半"等价**（对半分 = 剩余大小/剩余线程数），但需要维护速度衰减因子，**多一个可调参数就多一处可能调错**。**优先做简单的** |
| **抢正在下载的段（MP-H2 式）** | 需要 HTTP/2 改 byte range，**且要保证不重复下载已收字节** —— 风险远大于收益 |
| **预切更小（MIN_PIECE 降到 256KB）** | **握手开销占比上升**（今天的实测：22MB 配 32 连接已被 1MB 下限卡住）。**切碎不是解法** |

### 与其他决策的关系

- **`with_conns`（`afb7226`）解决的是"片数够不够"** —— 让每个连接有活干
- **本篇解决的是"干得均不均"** —— 让慢连接不要被分到大片拖尾
- **两者不冲突，是互补的**：先保证有片（`with_conns`），再保证公平（对半切）

## Acceptance criteria

1. **可观测**：日志能看出"片在下载过程中变小"（现在只在开始时打一行 `分片：N 片`；
   需要新增"切分事件"的日志，或统计 `切分次数`）
2. **不切碎**：任何时刻片大小 `> MIN_PIECE`（对半后两半都 ≥ MIN_PIECE 才切）
3. **不重复下载**：切分后 `done_bytes` 的累计必须仍等于实际收到字节（用单测验：
   切分后 `pending()` 覆盖的字节数 + `done_bytes()` == 文件大小）
4. **不打断在传的段**：只切"未领取"的片（队列里的），**不碰正在进行的 Range 请求**
5. **回归**：`cargo test --lib` 全绿；真机下载一个大文件（>100 MB）能完成且 sha1 正确

## Risks

| 风险 | 说明 | 缓解 |
|---|---|---|
| **切分逻辑与 `pending()`/`done_bytes()` 不一致** | 片表是位图 + 区间，切分要同时更新两处 | **单测直接验"切分前后覆盖的字节总数不变"**（Acceptance 3）|
| **频繁切分的开销** | 每次切分都要锁位图 | 切分只在"取片"时发生（每 worker 一次），频率天然很低 |
| **小文件反而变慢** | 对半会让片数变多 ⇒ 握手次数上升 | 只在 `> 2 × MIN_PIECE` 时切，且小文件本来就不分片（`NO_SPLIT_BELOW`）|
| **过度设计** | 本项目主要下 MC 文件（100KB–30MB 库 + 几十 MB client jar），**提速空间有限** | **先做，但用真机 A/B 测（同 URL 同时段各 3 次）**；**若提升 <5%，如实记录"不值得"** |
