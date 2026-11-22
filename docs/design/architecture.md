# 架构

当前状态（0.6.0）。历史任务书在 [archive/](archive/)：`2026-10-task-book-v0.4.md`（分片下载的设计与实测）、
`2026-10-direction-task-book.md`（从「MC 下载器」转向「通用下载器」）。**那两个文件是过程记录，不是现状**。

## 一、一句话

把「Minecraft 相关文件放到正确位置」做成可脚本化、可复现、可审计的命令行工具。
**下载源由用户提供**（`[source]` 六个键），本工具不含任何默认地址。

不做的三件事（定位边界，不是"还没做"）：不做启动器（不启动游戏、不管账号）、不做 GUI、不做浏览器插件。
理由在 [AGENTS.md](../../AGENTS.md) 第一节。

## 二、模块地图

```
入口层
  main.rs        clap 解析、全局参数、把 CLI 折成一层 Settings、分派子命令
  settings.rs    配置合并（CLI > 文件 > 默认）、[source] 解析、平台默认路径
  config.rs      下载脚本 [[task]] 结构 + schema 校验 + 逐任务覆盖 (Tune)

传输层
  download.rs    ★ 引擎：client / one / one_split / one_stream / fetch_piece /
                   TaskGroup（Drop 即 abort） / AdaptiveGate（RAII 并发闸门） / StallGuard
  scheduler.rs   任务调度；source.rs 多源池（EWMA 选源 + 失联降权）
  piece.rs       分片切片与续传状态（.part.state 位图，页对齐）
  progress.rs    进度行 + 限速令牌桶（全局带宽，不是每连接一份）

装配层
  vanilla.rs     version manifest → version.json → libraries / assets / client jar
  fabric.rs / forge.rs / neoforge.rs    三种加载器（版本映射与落后处理各不相同）
  pack/          Modrinth / CurseForge 整合包（overrides、禁分发清单）

工具层
  pathsafe.rs    落点安全：check_name / check_rel / under_root
  url_list.rs    URL 列表解析；单 URL → Job 的唯一推导口
  httpname.rs    Content-Disposition → 文件名（过 pathsafe）
  motw.rs        Windows Mark-of-the-Web
```

★ 下载引擎与进度行是最容易改出问题的地方，动之前先读 `.agents/notes/`。

## 三、一次下载的数据流

```
Job{url,path,sha1,size,mirrors}
  │  大小已知且 ≥8 MiB ⇒ 分片
  ├─ one_split: PieceTable::with_conns(size, piece_conns)
  │     └─ N 个 worker，各自循环：pick 源 → fetch_piece → 校验 Content-Range → 写指定偏移
  │           失败 ⇒ 退避重试（1<<attempt），换源；Range 回 200 ⇒ RangeUnsupported ⇒ 降级
  └─ one_stream: 整份流式写，按 Content-Length 或 EOF 收尾
       每步：限速令牌桶 → 落盘 → 进度 → StallGuard 检查推进
  收尾：SHA1 校验（有校验和才比）→ 删 .part / .part.state → MotW
```

三条不变量（破了就是坏文件，不是慢）：

1. **片起点与片长都是 4 KiB 的倍数** —— 与机械盘/页缓存对齐有关，见 `architecture/piece-page-alignment` note；
2. **续传判据必须用同一个 `--piece-conns`** —— 片大小是 `conns` 的函数，判据错就静默从头重下；
3. **`Content-Range` 的起点、终点、总长三项都要对** —— 只有总长对比是抓不到起点错位的。

## 四、借鉴了哪些机制（以及哪些只是类比）

| 借来的想法 | 来源（公开） | 落在我们哪里 |
|---|---|---|
| 固定大小块 + 块表 | PagedAttention（Kwon et al., 2023, vLLM） | `piece.rs` 的固定片大小与位图式完成表 |
| 连续批处理：任务不等整批 | Orca（Yu et al., 2022）；vLLM 的 continuous batching | `scheduler.rs` 的惰性派发 |
| 按实测负载分配 | EPLB（`deepseek-ai/EPLB`，公开仓库） | `source.rs` 的 EWMA 选源与降权 |
| 分块预填充 | Sarathi-Serve / vLLM chunked prefill | 大文件按 Range 切片 |

**边界说明**：这里借的是**想法**，不是数据结构。我们有位图式完成表，是因为不需要共享块、
不需要分配顺序；对照 vLLM 的 block manager（公开源码里是带 `ref_count` 的空闲表）时发现过
"把类比当移植"的错误，记在 `archive/2026-10-task-book-v0.4.md` 的 §二。

不迁移的：GPU/pinned memory、KV cache 压缩、投机解码 —— 与下载无同构之处。

## 五、已知取舍

| 取舍 | 为什么 | 代价 |
|---|---|---|
| 不内置源 | 合规（见 `AGENTS.md` 铁律 1） | 首次使用必须先配 `[source]` |
| 不做自动换源探测 | 同上：探测＝内置候选 | 用户要自己挑源（`probe` 子命令帮测速） |
| `--if-range` 默认关 | 多镜像 ETag 不一致，会把"能用但慢"的镜像判死 | 远端中途被换时只能靠 416/SHA1 发现 |
| 停滞检测而非总时限 | 总时限会掐死大文件的合法长传输 | 慢滴场景总耗时约 3 倍窗口（仍走重试） |
| CurseForge 禁分发模组只出清单 | 法律边界 | 用户需手动下载那几个文件 |

## 六、验证怎么算数

判据分三层，缺一层就不算验证过：

1. **单测/参数化/属性**（`cargo test`）：固化已知失败模式，186 条 lib + 40 条集成；
2. **假服务器**（`tests/http_semantics.rs`）：造截断、谎报长度、416、Range 回 200 这类真实故障；
3. **定量与真实网络**（`docs/reports/`）：测默认值合不合理，而不是"对不对"。

**"结构对"不等于"真的能跑"**：README 的验证状态表把两者分开写，含糊过就出过事。
