# Agent Note: 片边界对齐到 4 KiB（不是「HDD 友好」，是三条可测的小事）

Status: implemented

## Problem

下载器把每个分片**整片按偏移写出**：`fetch_piece` 收完一片后在 `spawn_blocking` 里
`open → seek(pj.start) → write_all(整片)`。所以**片起点就是写偏移**。

而片大小是 `clamp(size / piece_conns, 1 MiB, 16 MiB)`，除不尽时（例：96 MiB / 7 = 14 380 470 B）
片起点落在页/扇区中间。由此有三件事：

1. 相邻两片的**首尾共享同一个页缓存页**，两个 worker 会短暂争同一页的锁；
2. 片首尾的写**不足整页/整扇区**，块层下发时会出现非对齐 I/O（Windows 的 `Split IO/Sec` 可观察）；
3. 将来若要做直写（`O_DIRECT` / `FILE_FLAG_NO_BUFFERING`），偏移与长度都必须是页的整数倍 ——
   现在不对齐，等于把那条路提前堵死。

顺带暴露一个更严重的**静默缺陷**：`PieceState::load(part, size)` 固定用 `PieceTable::new`
（conns = 4）重建判据，而下载路径用 `with_conns(size, opts.piece_conns)`。片大小是
`size / conns` 的函数 ⇒ `--piece-conns` 不是 4 时，续传把自己的状态判成过期、**静默从头重下**，
不报错、不提示。

## Decision

1. 新增 `PAGE = 4096`；片大小在夹逼**之后向下取整到页**（`floor_to_page`）。
   向下而非向上：片变小只会让片数变多，「片数 ≥ conns」的契约只会更稳；向上取整会压片数，
   正好与契约相反。取整不会越界：`MIN_PIECE = 1 MiB`、`MAX_PIECE = 16 MiB` 本身就是页的整数倍。
2. 续传改为 `PieceState::load_with_conns(part, size, opts.piece_conns)`；
   新增 `PieceTable::restore_with_conns`。`load()` / `restore()` 保留为 conns = 4 的薄包装。
3. `.part.state` **不 bump magic**：`load` 已经逐项对照 `file_size / piece_size / total`，
   片边界一致则位图含义一致，旧进度继续有效；边界变了（公式改 / conns 变）自动作废重下。
   bump magic 会把「边界其实没变」的有效进度也一起丢掉。

## Alternatives considered

| 方案 | 为什么不选 |
|---|---|
| **不对齐（保持现状）** | 片边界落在页中间，块层必然出现非对齐下发；直写这条路等于提前封死。改动只有几十行，没有理由不做 |
| **向上取整到页** | 会让片变少，与「片数 ≥ conns」的契约相反；向下取整只会更安全 |
| **把对齐做到磁道 / 大 extent 粒度**（Traxtents, FAST '02 的做法） | 那是 HDD 的真正旋钮，但它要知道盘的实际几何（磁道大小、簇布局）并改分配策略；本项目跨平台、文件多在几十 MB 量级。**记录、本轮不做**（写顺序与预分配另见 issue #8） |
| **改用 `O_DIRECT` / `FILE_FLAG_NO_BUFFERING`** | `tokio::File` 不支持；且要求缓冲区地址也按页对齐（得自管内存），对 100 KB–30 MB 的文件收益抵不上篡改 I/O 路径的风险 |
| **bump `.part.state` magic** | 见 Decision 第 3 条：会把边界没变的有效进度也丢掉 |

## Consequences

- 片大小会比原来小 0–4095 B（最多少一页），片数最多多一片 —— 对吞吐无实质影响。
- 旧的未下完大文件：若新公式算出的片边界与旧状态不同，续传会作废、从头下（已有行为，不是新增损失）。
- `--piece-conns` 不是 4 时，续传**从此真的能续**（此前是静默失效）。
- 写顺序（乱序写、NTFS 的 VDL 零填充、预分配）**本轮未解决**：见 issue #8 与
  `proposed/architecture/2026-11-22-write-locality.md`。
- 单测证据：`piece_size_is_page_aligned`、`every_piece_starts_on_page_boundary`、
  `floor_to_page_rounds_down_and_keeps_the_floor`、`state_roundtrip_preserves_non_default_conns`、
  `state_with_unaligned_piece_size_is_rejected`（2026-11-22 核：`cargo test --lib` 138 passed ——
139 减去随后删掉的两条重复 `split_for` 用例、加上 `line_never_announces_completion`）。
- 实测证据（strace 前后对照，Kali/Linux，96 MiB + `--piece-conns 7`）：
  **改前 6/7 个 `lseek` 偏移不是 4096 的倍数**（14,380,470 / 28,760,940 / 43,141,410 …，余数 3510、2924、2338…），
  **改后 0/8 违规**（14,376,960 = 3510×4096；末片 24,576 B 止于 EOF，起点仍对齐）。
  墙钟差异在噪声内（0.51 s vs 0.58 s，n=3）——**这也正是"只能是卫生"的证据**。
  完整数据、口径与限制：`../../../docs/reports/quant-report-20261122.md` §4（E3）。**数字以那份报告为准**，本 note 只记结论。
