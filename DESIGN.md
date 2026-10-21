# mc-downloader 任务书（v0.4 分片下载）

> 2026-10-21 ｜ 状态：accepted ｜ 依据：与 lmq 关于「DeepSeek 调度/缓存能否迁移到下载器」的讨论
> 基线：Rust 1.99.1 · Edition 2024

---

## 一、定位（先说清不做什么）

**不是一个「通用下载器」**，不在浏览器集成、站点嗅探上与 IDM 正面竞争。

**是「分片式、可脚本化、面向大文件分发」的下载器**——IDM 做不了的三件事：

1. **分片级断点续传**（不是整文件重下）
2. **单文件多连接**（大文件可被 N 个连接同时拉）
3. **多源动态分配**（同文件多镜像，按实测速度分片）

**不做什么**：

| ❌ 不做 | 理由 |
|---|---|
| GUI | 目标用户是脚本/CI |
| 浏览器集成 | IDM 的护城河，投入产出比极低 |
| HTTP/3 / QUIC | v0.5 再议，先把分片跑通 |
| 非 HTTP 协议（FTP/S3/BT）| 只做 HTTP/HTTPS |
| GPU 加速 | **下载是 I/O 密集，瓶颈在网络**；GPU 不能发网络请求，多一次 PCIe 往返反而更慢 |

---

## 二、借鉴了 DeepSeek 的哪些机制

| DeepInfer / DeepSpec 机制 | 迁移到下载器 | 落点 |
|---|---|---|
| **PagedAttention 的「固定大小块」** | **固定大小分片 + 按片管理** | `piece.rs` |
| **Continuous batching** | **全局分片队列，新任务随时插入** | `scheduler.rs` |
| **EPLB（专家负载均衡）** | **多镜像按实测吞吐分配分片** | `source.rs` |
| **chunked prefill** | **大文件 Range 分片** | `download.rs` |
| ~~ZOS（pinned memory）~~ | ❌ 不迁移 | GPU 专属，下载器无 CPU↔GPU 层 |
| ~~KV cache 压缩 / 投机解码~~ | ❌ 不迁移 | 与下载无关 |

**核心判断**：LLM 推理与下载器同构的地方是「**把大任务切小、动态调度、平衡负载**」，不同构的是 GPU 相关的全部机制。

> ⚠️ **2026-10-21 核对源码后修正（原文写错了）**
>
> 原文这条写的是「PagedAttention + **BlockManager** → 固定大小分片 + **位图状态表**」。
> 实际读 `DeepInfer/csrc/engine/block_manager.hpp`：
> `struct Blocks` 是**双向链表 free list**（`prev`/`next`/`_head`/`_tail`）+ **`ref_count`**
> （支持跨 sequence 共享同一块），**不是位图**。
>
> 我们的 `PieceTable` 是位图，只记「哪片完成」——**比它简单，因为不需要共享、不需要分配顺序**。
>
> ⇒ **借的是「固定大小块」这个概念，不是它的数据结构。**
> 把「概念借用」写成「结构移植」，是**把类比当成了移植**。
>
> 另外三条（Continuous batching / EPLB / chunked prefill）**本轮未逐条核对源码**，
> 引用时按同样标准重新验。

---

## 三、v0.4 范围

### 3.1 设计

```
文件 → 切成固定 4 MB 分片 → 位图记录状态 → 全局队列调度 → 多连接拉取
```

**PieceTable**（学 block table）：

```rust
struct PieceTable {
    piece_size: u64,      // 默认 4 MB，按文件大小自适应
    total: u32,           // 分片数
    bitmap: Vec<u64>,     // 每 bit 一片：0 未完成 / 1 完成
    inflight: HashSet<u32>,
}
```

**位图开销**：10 GB / 4 MB = 2560 片 ⇒ **320 字节**。

**分片大小自适应**（学 aria2 的 `min-split-size`）：

| 文件大小 | 分片 | 理由 |
|---|---|---|
| < 8 MB | **不分片**（单连接）| 握手开销 > 收益（MC 模组全在这个区间）|
| 8 MB – 256 MB | 4 MB | |
| > 256 MB | 16 MB | 减少分片数量，降低调度开销 |

**⚠️ 这条是本设计的核心**：**小文件不分片**——不是偷懒，是 aria2 的默认策略（`min-split-size=20M`），也是 MC 场景的实际情况。

### 3.2 落点

```
src/
├── piece.rs       新增：PieceTable + 状态机
├── scheduler.rs   新增：全局分片队列（动态插入）
├── source.rs      新增：多源管理（速度评分 + 分片分配）
├── download.rs    改造：单文件下载改为「按分片下载」
├── version.rs     Job 增加 size / mirrors 字段
└── main.rs        CLI 不变（可加 --pieces / --connections）
```

### 3.3 状态机（单片）

```
Pending ──取到──> Inflight ──成功──> Done
   ↑                 │
   └────失败─────────┘（重试 N 次后 Failed，整任务失败）
```

**失败只退回该片**，不影响其它片。

---

## 四、验收标准（可执行，不是「看起来对」）

| # | 验收项 | 怎么验 |
|---|---|---|
| 1 | **小文件不分片** | `vanilla 1.7.10` 的 721 个文件，分片数全为 1 |
| 2 | **大文件分片** | 26.3 的 `assets` 大文件，分片数 > 1 |
| 3 | **分片级断点** | 下到一半 kill，重跑只补缺失分片（对比 `.part` 文件大小）|
| 4 | **单文件多连接** | 大文件下载时 `netstat` 可见 N 条连接 |
| 5 | **多源切换** | 给一个文件两个镜像（一快一慢），慢源分片数明显少于快源 |
| 6 | **功能不退化** | `vanilla 1.7.10` / `vanilla 26.3` / `pack 26.1.2-Fabric` 三例全部跑通，SHA1 全对 |
| 7 | **无 .part 残留** | 成功后目录里无 `*.part` |

**现有 SHA1 校验、重试、代理全部保留。**

---

## 五、实施顺序

```
① piece.rs       数据结构 + 位图（不接网络，纯单测）
② scheduler.rs   队列 + 并发控制（假下载器测）
③ download.rs    接入分片（单源）
④ source.rs      多源（一个文件多 URL）
⑤ 回归三例       vanilla 1.7.10 / 26.3 / pack
```

**每步可独立验证；④ 如果收益不明显可以砍掉，①②③ 是主干。**

---

## 六、风险与取舍

| 风险 | 处理 |
|---|---|
| **MC 场景大部分文件 < 8 MB** | **不分片**，v0.4 对 MC 下载**几乎没有加速**——这是设计选择，不是缺陷 |
| 分片让请求数变多 | 只在 > 8 MB 时才分片，且分片 ≥ 4 MB |
| 多源可能拿到不同版本 | **每片仍逐个 SHA1 校验**（校验的是整文件，不是分片）|
| 索引文件的 SHA1 是全文件 | 分片下载完成后**合并再整体校验**，失败则整文件重下 |

**坦白**：**v0.4 对 mc-downloader 本身收益有限**（它的文件都小）。**真正的价值在「给大文件分发场景打底」**——这也是「不做通用下载器、要做特定场景」的落地。

### 实测数据（2026-10-21，26.3 全量）

```
总文件数      5224
体积          assets 475 MB · libraries 84 MB · versions 40 MB
超过 8 MB     仅 5 个（0.096%）
              assets/objects/85/853da…
              libraries/com/ibm/icu/icu4j/78.3/icu4j-78.3.jar
              libraries/it/unimi/dsi/fastutil/8.5.18/fastutil-8.5.18.jar
              libraries/org/lwjgl/lwjgl-vulkan/3.4.3/lwjgl-vulkan-3.4.3.jar
              versions/26.3/26.3.jar
```

**5224 个文件里只有 5 个走分片** —— **0.096%**。这是对"MC 场景收益有限"最直接的证据。

**校验**：`26.3.jar` 41,483,720 字节，SHA1 与 manifest 一致；无 `.part` 残留。

### 大文件实测（2026-10-21，Ubuntu Server 24.04 ISO）

```
文件       ubuntu-24.04.3-live-server-amd64.iso（清华镜像）
大小       3,303,444,480 字节（3.08 GB）
分片       197 片 × 16 MB（走 PIECE_LARGE 档）
并发       4
耗时       约 2 分钟（11:01 → 11:03），均速约 26 MB/s
SHA256     本地 c3514bf0…274b == 官方 SHA256SUMS 同值  ✅
.part      无残留
```

**结论**：3 GB 单文件、197 片、4 并发，**逐字节正确**。这条路径在大文件上是可用的。

---

## 七、参考实现

| 参考 | 学什么 | 位置 |
|---|---|---|
| **aria2** | `min-split-size` / `--max-tries` / 分片调度 | `importantClone/aria2` |
| **DeepInfer** | block table / 动态队列 / 负载均衡 | `DEEPSEEKCLONE/DeepInfer` |
| **mc-downloader v0.3** | 流式写盘 / SHA1 边写边算 / `.part` + rename | 本仓 |

---

## 八、已知待办与已落地项

### 8.1 多源分配（已实现，2026-10-21）

**状态**：✅ 已落地（`src/source.rs`）。**在 MC 场景仍然用不上**（每个文件只有一条官方 URL），
但 CLI 已支持 `--mirror`，对大文件分发场景可用。

**实测**：Ubuntu 24.04 ISO（3.08 GB），清华单源 vs 清华+中科大双源，
两者 SHA256 均为 `c3514bf0…`（与官方一致）。**功能正确**；速度对比受限于测量误差未取到精确值。

**实现**（`src/source.rs`）：

```
同一文件的 mirrors: Vec<String>
每个源维护 ewma_throughput（指数滑动平均）
分配规则：
  · 初始均分
  · 每完成一片，更新该源吞吐
  · 下一片优先分给吞吐最高的源
  · 连续失败 N 次的源降权 / 摘除
```

**落点**：`source.rs`（`Job.mirrors: Vec<String>` · `PieceJob.source_idx: usize`）。

**ewma 参数**：α=0.3 · 初始 8 MB/s · 失败 ×0.5 · 下限 64 KB/s。

**验收**：6 个单测覆盖（初始选择 / 快源胜出 / 失败降权 / 下限 / 恢复 / 空池安全）。

### 8.2 其他

#### A. 已知代码缺口（按严重度，2026-10-21 依据文献复核）

| 项 | 说明 |
|---|---|
| **完整性链缺口（三层）** | 依据 *Survivable Key Compromise in Software Update Systems*（CCS'10，被引 172）——软件更新必须认证**三类**信息：<br>① **内容层**：`version_manifest_v2.versions[].sha1` 与 `assetIndex.sha1/size` 官方都给了，我们的结构体没声明 → 白丢校验（改 3–4 行可补）<br>② **时效层（timeliness）**：**完全没有**。无版本号比较 ⇒ 攻击者塞一个旧 manifest，我们无从发现（回滚攻击）<br>③ **一致层（consistency）**：没有。无法发现"不同用户拿到不同版本"（split-view）<br>⚠️ **补两个 sha1 只修了①**，②③ 是设计级空白 |
| **续传不带 `If-Range`** | `download.rs:527` 只发 `Range`，**无 ETag / Last-Modified 校验**。服务器上文件换了（MC 版本更新 / 镜像同步滞后 / CDN 缓存不一致）时，会把**旧前段 + 新后段**拼成一个跨版本的混合文件：有 sha1 能抓但要整文件重下；**`fetch` / `generic` 无 sha1 则静默交付坏文件**。<br>标准解法 `If-Range: <strong ETag>`：校验器变了让服务器回 200 全量，客户端从头下而不是拼接 |
| **`--adaptive` 的减半分支在**高并发**下不可达**<br>✅ **已实测（2026-10-21）** | 实测（1GB / 8mbit / adaptive / 25s 窗口，**只改 `--piece-conns`**）：<br><br>| conns | tune 行数 | add | **cut** | max(avail) |<br>|---|---|---|---|---|<br>| **1** | 12 | 6 | **4** | 2 |<br>| **2** | 10 | 6 | **3** | 2 |<br>| 4 | 9 | 6 | **0** | 1 |<br>| **16** | 9 | 5 | **0** | 0 |<br><br>⚠️ **这不是「死代码」**（conns=1/2 时 cut 正常工作）。准确结论：<br>`cut` 需要 `avail > 1`，即 `permits > 2×conns + 1`；而 `permits` 从 `conns` 起、每 2s 才 +1 ⇒ **25s 窗口内要 `conns < 11` 才够**。<br>⇒ **并发越高，减半越不可达 —— 而高并发恰恰是最需要减半的场景。**<br>**修法方向**：减半条件不应依赖 `avail`（它衡量"有多少空闲许可"，与"该不该减"无关），改成「当前并发 > 1」。 |

#### B. 协议与实现待办

| 项 | 说明 |
|---|---|
| 断点状态持久化 | ✅ **已落地（v0.5，见 §11.4）**：位图旁挂 `<name>.part.state`，进程被杀后重跑只补缺口 |
| 分片级 SHA1 | 现在校验整文件（正确但需等全部下完）；Modrinth/原版 manifest 都只有全文件 SHA1，分片级需自建索引 |
| HTTP/3（QUIC）| 需要 `quinn`，且要求服务端支持；当前 CDN 多数只到 h2 |
| `one_split` 仍偏长 | 已从 8 参数降到 6，但函数体仍有 ~200 行（建表/预分配/源池/调度/worker/tuner/校验/落盘）。下一轮可再拆成 `prepare` / `run_workers` / `finalize` 三段 |
| CurseForge 格式 | 已能识别（`manifest.json`）并给出明确报错，未实现解析 |
| Forge / NeoForge | 已能识别并报出要求版本，未实现（需跑官方 installer） |
| 整合包多 URL | mrpack 的 `downloads[]` 是多 CDN 镜像，目前只取第一条、其余丢弃。要接得先测「按源分片」是否真比单源快 |

#### C. 做对应功能时再读的参考

| 场景 | 参考 | 为什么 |
|---|---|---|
| **T5.1 的 AIMD 阶跃** | *TheaterQ: A Qdisc for Dynamic Network Emulation*（2510.27057） | `tc del/add` 切换带宽**中间有间隙**，会污染测量。学术上已把「NetEm 静态参数」记为已知局限，TheaterQ 用 trace 文件驱动动态仿真解决。<br>**本项目先用轻量缓解**：段间留 2s 稳定期 + 只取段末 10s 统计，不引第三方 qdisc |
| **多源调度** | *MDTP — An Adaptive Multi-Source Data Transfer Protocol*（2505.09597） | 自适应多源传输，与我们的 `--adaptive` + `mirrors` 同方向。等真的开始测多源时再读 |
| 评估方法本身 | *Emulation vs Simulation*（2608.02095） | 拥塞控制类评估"性能取决于算法/传输栈/应用行为/测量过程的交互"——提醒我们**测出来的可能是环境不是代码** |


#### D. 依赖选型的待评估项

| 项 | 说明 |
|---|---|
| **`reqwest` 默认 `native-tls` → 需要 OpenSSL 头文件** | 2026-10-21 在 Kali VM 上撞到：只有运行时 `libssl.so.3`、无 `libssl-dev`，`cargo build` 直接失败（`Could not find directory of OpenSSL installation`）。CI 上能过是因为 ubuntu-latest 自带 `libssl-dev`。<br>**建议评估切 `rustls-tls`**：`default-features = false, features = ["json", "stream", "rustls-tls"]`。<br>收益：去掉 C 依赖 ⇒ 交叉编译到 `x86_64-unknown-linux-musl`（静态单文件）变得可行、任何 Linux 都能直接编译。<br>代价：需确认证书验证行为差异（rustls 更严）与根证书来源（`webpki-roots` vs 系统）。<br>⚠️ 这会改 TLS 栈，属**行为变更**，需要单独一轮 + 审查 |

---

## 九、高性能复制器（待评估）

**观察**：复制与下载是同一类问题——**I/O 密集、瓶颈在介质带宽**，只是复制多一跳（读源 + 写目标），且没有网络延迟。

**Windows 自带复制慢的真实原因**（不是"没多线程"）：

- 小文件多时，**每个文件的 open/close 与元数据操作**是主要开销
- 默认带缓冲复制在机械盘 / 网络盘上会**反复分配缓冲**
- **没有利用 `CopyFileEx` 的内核态快速路径**（同卷复制可走 reflink/COW）

**可行方向**（若要做，独立子命令 `copy`，不混进下载路径）：

| 优化 | 手段 |
|---|---|
| **大文件** | `std::fs::copy`（Windows 走 `CopyFileEx`）+ 可选 `copy_file_range`（Linux）|
| **小文件海量** | 多线程 worker + 目录树预扫描，按大小排序 |
| **同盘复制** | 探测 reflink 支持（NTFS 的 `FSCTL_DUPLICATE_EXTENTS`、APFS/Btrfs 的 clone），可做到"秒完成" |
| **校验** | 复用本仓 `sha1_file`（blocking 线程池）|
| **进度** | 复用同一套记速器 |

**判断**：**这是另一个产品，不是 mc-downloader 的一部分**。要不要做取决于是否有真实痛点（比如经常搬几十 GB 的数据集）。

**如果做**，建议**独立 crate**（`fastcp`），共享 `piece` / `scheduler` / `progress` 三个模块。

---

## 十、内置记速器（v0.4.1）

**现状**：只有"第 n/总 个文件"计数，**看不到速度和剩余时间**。

**设计**：

```rust
pub struct Progress {
    done: AtomicU64,        // 已传输字节
    total: AtomicU64,       // 总字节（未知时为 0）
    start: Instant,
}
// 后台任务每秒打印：
//   [  12.3 MB / 39.6 MB ]  8.4 MB/s  ETA 00:03
```

**关键**：**用原子变量而非锁**——16 个并发任务每写一块都更新，加锁会成为新的瓶颈。

**落点**：`progress.rs`，`download::run` 里创建并每秒打印一次；`one_split` 的 `fetch_piece` 也要 `add`。

---

## 十一、v0.5 工程化重构（2026-10-21 已落地）

> 背景：功能冻结，进入长期维护。新增能力基本只剩「更多整合包格式 / 加载器」，
> 所以这次重构**只留那两处接缝**，其余一律不动。

### 11.1 crate 形态：先拆 lib（其余改动的**前提**）

原先是纯 bin crate ⇒ 模块是私有的 ⇒ 集成测试无从下手。
拆成 `src/lib.rs`（11 个 pub mod）+ `src/main.rs`（只做 CLI 解析与分派）之后，
此前 **929 行零测试**的 IO 密集代码第一次可测。

> ⚠️ **副作用要记住**：`pub` 项在 lib 里不受 `dead_code` 检查。
> 拆完 clippy 告警从 8 条掉到 2 条，**那是告警被藏起来，不是问题被解决**。
> 清死代码得靠人工 grep，不能靠 clippy。

### 11.2 模块地图

| 层 | 文件 | 职责 |
|---|---|---|
| 入口 | `main.rs` | clap 解析、参数优先级合并、任务分派 |
| 编排 | `vanilla.rs` `pack/mod.rs` | 各场景的流水线 |
| 下载 | `download.rs` | 流式/分片分流、重试、多源、进度 |
| 分片状态 | `piece.rs` | `PieceTable`（位图）+ `PieceState`（落盘） |
| 调度 | `scheduler.rs` | `VecDeque` + `Notify` 的分片队列 |
| 源选择 | `source.rs` | ewma 吞吐 + 失败降权 |
| 格式解析 | `pack/mrpack.rs` `version.rs` `manifest.rs` | 外部 JSON → 内部结构 |
| 加载器 | `pack/loader.rs` `fabric.rs` | 加载器 → 可启动的 `versions/<id>/` |

**参数一律收进 `download::DownloadOpts`**（proxy / retries / concurrency /
piece_conns / adaptive / timeout_secs）。别再往下透传裸参数——
之前那串 5 个参数在 4 层函数里手工搬运，clippy 报过两次 `too many arguments`。

### 11.3 整合包：两处接缝 + 一个中间表示

```
归档 ──parse──▶ PackSpec ──▶ 原版 ──▶ 加载器 ──▶ 文件 ──▶ 覆盖层
     (可插拔)   (中间表示)                          (不变)
```

- **加一种格式** ⇒ 在 `pack/mod.rs::parse` 加一个分支 + 一个 parser，输出 `PackSpec`
- **加一种加载器** ⇒ 在 `pack/spec.rs` 加 `Loader` 变体 + 在 `pack/loader.rs` 加实现
- **两者互不影响**；中间三段（原版/文件/覆盖层）对所有格式都一样，别为了复用去改

**识别 ≠ 支持**：`mrpack.rs` 能认出 Forge/NeoForge（报错带版本号），
但 `Loader::check_supported()` 必须在**任何下载开始之前**调用——
否则要下完几个 GB 才说「不支持」。

### 11.4 分片级断点续传（v0.4 设计了但没接电）

`PieceTable` 的位图状态机一直存在、8 个测试一直绿，但生产代码从没读过它 ⇒
`.part` 里已下好的分片重跑时全部重下。v0.5 补上接线：

- `one_split` 用 `table.pending()` 只排未完成片段，每片成功 `mark_done`
- 位图旁挂落盘到 `<name>.part.state`，每 2 秒快照（崩溃最多丢 2 秒进度）
- **不能靠稀疏文件推断已完成片段**：`set_len` 的空洞读出来是 0，
  与「真的写入了 0 值字节」无法区分 —— 这是必须旁挂状态文件的唯一原因
- 加载时逐项校验 `file_size / piece_size / total / 长度`，任一不符即作废重下；
  宁可重下，不要拼出坏文件
- 成品落盘后删除状态文件，避免与下一次的 `.part` 配错

**状态文件格式**（小端，32 字节头 + 位图）：

| 偏移 | 长度 | 内容 |
|---|---|---|
| 0 | 8 | magic `MCDLPC01` |
| 8 | 8 | `file_size` |
| 16 | 8 | `piece_size` |
| 24 | 4 | `total`（分片数） |
| 28 | 4 | `words`（位图 u64 个数） |
| 32 | words×8 | 位图，每 bit 一片 |

### 11.5 工程闸门与语法基线

CI 三道闸（`.github/workflows/ci.yml`）：

```bash
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
cargo build --release --locked
```

配套：`.gitattributes` 全仓 `eol=lf`（此前 6 个文件是 CRLF，混排会让 rustfmt
的 diff 变成整文件重写）。

> ⚠️ **刻意不用 `rust-toolchain.toml` 钉 channel**：那会让 rustup 每次去同步
> channel 元数据，离线或代理受限时**连 cargo 都跑不起来**（本机实测）。

**语法基线分三层，缺一层就只是「允许」而不是「要求」**：

| 层 | 声明位置 | 作用 |
|---|---|---|
| 编译器 | `Cargo.toml` `rust-version = "1.99"` | cargo 在编译器过旧时直接拒绝编译 |
| 语言版本 | `Cargo.toml` `edition = "2024"` | 允许 2024 语法与语义 |
| **强制 lint** | `Cargo.toml` `[lints.rust]` / `[lints.clippy]` | **禁止退回旧写法** |

`[lints]` 里的几条不是风格洁癖，各有明确目的：

| lint | 挡住什么 |
|---|---|
| `unsafe_code = "forbid"` | 全仓 unsafe。将来真要加（mmap / SIMD），必须先改这行 —— 强制过一次讨论 |
| `rust_2024_compatibility` (group, `deny`) | if-let 临时值作用域、尾表达式 drop 顺序、RPIT 捕获等十几条迁移 lint。写出在 2021/2024 下**行为不同**的代码会直接编译失败 |
| `keyword_idents_2024 = "deny"` | `gen` 等新关键字被当标识符 |
| `unused_must_use = "deny"` | 忽略 `Result` |
| `clippy::incompatible_msrv = "deny"` | 用了比 `rust-version` 更新的 API —— 让基线自己守住，不靠人记 |
| `clippy::manual_let_else` | 把 `match` 降级成 `let-else` 的地方 |
| `clippy::redundant_clone` / `needless_pass_by_value` | 多余复制与所有权浪费 |

> **为什么放 `Cargo.toml` 而不是 `lib.rs` 的 `#![warn]`**：一处声明对
> lib / bin / test / bench 四个 target 同时生效，不会漏掉某一个。
>
> **group 必须带 `priority = -1`**，否则与单条 lint 同优先级会让 cargo 直接报错。

**已实测可用的 1.99 新 API**（写代码前先查证，不凭记忆）：

`Option::is_none_or` · `inspect` · `take_if` · `Result::is_ok_and` ·
`[T]::split_at_checked` · `slice::as_chunks` · `Vec::pop_if` ·
`Path::file_prefix` · `std::fs::exists` · `LazyLock` · `io::Error::other` ·
`array::from_fn` · `String::leak` · `#[expect(lint)]` · let chains（`if` 与 `while` 均可）·
`#[unsafe(no_mangle)]` · `unsafe extern`

**仍未稳定的**（别写）：`iter::next_chunk` · `slice::array_chunks`

⚠️ **反直觉的一条**：edition 2024 下 `&raw mut STATIC` **不需要** `unsafe` 块，
写了会被报 `unnecessary unsafe`。这条只有实际编译才发现，很容易凭记忆写错。

### 11.6 本机环境坑（会复发）

- **增量编译缓存会坏**：症状是 `cargo build --tests` 卡在 `Compiling` 直到超时，
  日志里先出现 `error deleting lock file ... os error 5 (拒绝访问)`。
  绕法：`CARGO_INCREMENTAL=0 cargo test`。
- **沙箱会掐停批量删除**：`cargo clean -p` 删到第 ~10 个文件即 SIGTERM，
  于是缓存永远是半删状态。别指望 `cargo clean` 修它。
- **`1.99.1` 这套 toolchain 是装坏的**（`rustup component list` 报
  `missing manifest`）；能用的是 `stable`，而 `stable` 恰好就是 1.99.1。
- **release 构建不是「本地跑不了」，是「固定路径被拦」**
  （2026-10-21 更正，此前记错过）：沙箱只拦 `<repo>/target/release` 这个路径，
  用 `CARGO_TARGET_DIR` 指到仓库外即可。

  ```bash
  CARGO_TARGET_DIR=/tmp/mcd-rel cargo build --release --locked
  CARGO_TARGET_DIR=/tmp/mcd-rel cargo test  --release --locked
  ```

  实测：产出 4.48 MB、`--version` 正常、`vanilla 1.7.10` 全缓存回归通过、
  **release 下 62 个测试同样全绿**。

  ⚠️ **判据：遇到「做不到」先问「是能力不行，还是某个具体路径被拦」** ——
  这两者的绕法完全不同，而后者往往换条路就通了。之前把它写成「本地无法验证」，
  于是把一个能 20 秒做完的验证推给了 CI，等到下一轮才发现是路径问题。

## 十二、真实网络验证（Kali VM + 真实源）

> 2026-10-21 立 ｜ 状态：**proposed，排在批次 6 之后**
> 目的：合成测试（`net_faults`：`lo` + 假源）证明"逻辑对"，**证明不了"在真网络上对"**。
> 这一节是验证策略的终点，不是替代。

### 12.1 合成测试证明不了什么

| 维度 | `lo` + 假源 | 真实链路 |
|---|---|---|
| TCP 拥塞行为 | `lo` 是 `noqueue`，没有 AQM、没有真实 RTT、没有真实 BDP | 真实慢启动、真实重排 |
| TLS 握手 | 几乎没有开销 | 真实往返 |
| CDN 行为 | 虚拟 | 就近接入、多 IP、缓存命中/未命中 |
| 磁盘 IO | 常落 tmpfs | 真实 fsync 延迟 |
| 长时稳定性 | 几十秒 | 几十分钟的 fd / 内存 / 句柄泄漏 |

### 12.2 三个层次的"真"

| 层 | 源 | 网络 | 能证明什么 |
|---|---|---|---|
| **L0 合成**（`net_faults`）| 假源（`lo`）| `netem`（`lo`）| 逻辑正确、判定可靠 |
| **L1 半真实**（本节）| **真实源**（Mojang / Modrinth CDN）| **`netem`（`eth0`）** | 真实协议栈 **+** 受控故障 |
| **L2 全真实** | 真实源 | 真实网络 | 基线性能（**不可控、不可复现**，只取一次参考值）|

**L2 不能作为验收依据**——它不可复现。但它能给出"我们离现实差多远"的参照。

### 12.3 L1 的做法

```
[公网真实源]  ←── 校园网 ──  [Kali VM eth0]──netem──[mc-downloader]
                              ↑ 故障只加在这里
```

- **客户端在 VM 内**，源在公网 ⇒ 真实 RTT（校园网 10–30ms）
- **`netem` 叠在真实链路上** ⇒ 真实基线 + 可控扰动
- **不改源、不改被测程序**（同 `net_faults` 的接缝原则）

#### 三条安全禁忌（都踩过或推演过）

1. **绝不在 `eth0` 上改 MTU**。
   `ip link set dev eth0 mtu …` 改错值 ⇒ **VM 失联**，只能重启。
   MTU 只在 `lo` 上动（`net_faults` 的做法）。`lib.sh` 的 `__NF_ARMED` 门就是为此加的。
2. **用 `tc qdisc replace`，不要 `del` + `add`**。
   `del` 与 `add` 之间有**空窗**，此时流量走默认 qdisc——阶跃测量会被污染。
   实测（2026-10-21，VM 上）：
   ```bash
   tc qdisc replace dev eth0 root netem limit 1000   # ✅ REPLACE-OK
   tc qdisc replace dev eth0 root fq_codel           # ✅ 恢复，网络仍通
   ```
   ⚠️ 恢复后 handle 会变（`0:` → `8016:`），**功能等价、参数一致**，不必强求 handle 相同。
3. **收尾必须恢复 `fq_codel`**。VM 的 `eth0` 默认是 `fq_codel`，恢复成 `noqueue` 会静默改变基线，
   后续所有"真实"测量都不可比。

#### 一条前提：先量真实带宽上限

`netem rate` 只有在**比真实链路窄**时才是瓶颈。若校园网只有 100 Mbit 而你设 `rate 400mbit`，
瓶颈在校园网，`netem` 形同虚设，测出来的"限速生效"是假象。

⇒ **R1 基线必须先跑**，用它确定各档 `rate` 的上限。

### 12.4 场景清单

| # | 场景 | 施加 | 判据 |
|---|---|---|---|
| **R1** | 真实源无故障基线 | 无 | 记录吞吐 / 耗时 / 连接数 / RTT，**作为 R2–R5 的对照** |
| **R2** | 真实源 + 限速 | `eth0` rate（≤ R1 实测值的一半）| 吞吐 ∈ `[0.8, 1.2]×`，sha1 一致 |
| **R3** | 真实源 + 丢包 | `eth0` loss 5% | 完成且 sha1 一致；**记录重试次数**（不只看结果）|
| **R4** | 真实源 + 中途 RST | `iptables` 断**出向** | **续传生效的正判据：第二轮下载字节 < 总字节**（只比 sha1 无法区分"续传"与"从头重下"，见 §8.2 与 `net_faults` T3.3）|
| **R5** | 真实源 + 带宽阶跃 | `eth0` rate 变化 | `--adaptive` 的 permit 变化能与吞吐变化对齐 |
| **R6** | 长时稳定性 | 无 | 连续 30 min 下多个版本，**fd 数 / RSS 不单调增长**；`.part*` 不累积 |

**R6 是合成测试完全覆盖不到的一条**——短测试看不到句柄泄漏与内存增长。

### 12.5 与 L0 的分工

| 问题 | 归 L0（`net_faults`）| 归 L1（本节）|
|---|---|---|
| 判定逻辑对不对（假 PASS）| ✅ | — |
| 分片偏移 / 续传状态机 | ✅ | — |
| 真实拥塞下的吞吐 | — | ✅ |
| 真实 CDN 的多 IP / 缓存 | — | ✅ |
| 长时资源泄漏 | — | ✅ |
| 平台语义（稀疏/大小写/rename）| ✅（走 CI）| — |

**判据：能合成的一律合成**（快、可复现、进 CI）；只有"必须真"的才上 VM。

### 12.6 验收

- 每一轮**必须留 R1 对照**，否则"变快了/变慢了"无法归因
- 结论必须能落到「**是我们的问题**还是**网络的问题**」——落不到的测量不算结论
- 不跑 L1 于每一轮改动；它在**发版前**跑一次即可（成本高、不可频繁）

---
