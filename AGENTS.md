# AGENTS.md —— mc-downloader

> **给改这个仓库的 AI 看**。用户向说明在 `README.md`；决策理由在 `.agents/notes/`。
> 三者分工：README 回答「怎么用」，本文件回答「改的时候不能踩什么」，notes 回答「为什么是现在这样」。

---

## 一、这个项目是什么（一句话）

把「Minecraft 相关文件放到正确位置」做成**可脚本化、可复现、可审计**的命令行工具。
**不做启动器**（启动/账号/存档交给 PCL、HMCL）。

**不做 GUI，也不做浏览器插件。** 不是"没来得及"，是定位边界：

- GUI：日常下载交给现成的下载器（个人用 FDM）；本工具的产出是"把文件放对位置"，
  不是"让人点着用"。加 GUI 会把「可脚本化」这个前提变成次要。
- 浏览器插件：需要 Native Messaging **常驻进程**守着，与「单次命令、跑完退出」
  的形态直接冲突。真要做，得先改成本体常驻 —— 那是另一个项目。
  参考实现见 `anefzaoui/misterdownloadmanager`（纯 Rust，egui + `extension/`，
  2026-10 起一个月 16k 星）。

---

## 二、铁律（改代码前必读，每条都有代价换来的理由）

| # | 铁律 | 为什么 |
|---|---|---|
| **1** | **不内置任何下载源**（官方或第三方都不内置）。源由用户在配置的 `[source]` 段提供；代码里**不得有默认地址兜底**，也**不得自动探测** | 2026-10-20 PCL2 移除 MC 下载功能（「为遵守相关法律法规」）——「第三方工具内置下载通道」处在合规灰区。判据是机器可查的：`src/` 里零命中（CI 有这一步） |
| **2** | **对 CurseForge 标记「禁止第三方分发」的模组，只生成 `manual.txt`，不取直链** | 这是法律边界，不是技术限制。 |
| **3** | **不提供账号验证绕过 / 离线破解 / 任何"非官方"下载源** | 见 `README.md` 的法律定位一节。 |
| **4** | **不启动游戏、不管理账号** | 定位边界。加了就变成"启动器"，那是另一个产品。 |
| **5** | **SHA1 校验失败必须重下，不许"跳过校验继续"** | 校验是唯一的完整性保证；放过一次，之后所有"校验通过"都失去意义。 |

---

## 三、构建与验证（判据都是"跑出来的"，不看 IDE 脸色）

```bash
cargo fmt --all                                     # ① 格式（pre-commit 门会拦）
cargo clippy --all-targets --locked -- -D warnings   # ② lint（-D warnings = 警告即错）
CARGO_INCREMENTAL=0 cargo test --lib --locked        # ③ 单测（lib 当前 186 条）
cargo build --release                               # ④ 产物：target/release/mc-downloader.exe
```

**注意：提交前必须跑完 ①②③**。这不是建议 —— **2026-10-25 曾因只跑「能编译 + 单测」而连续 5 次 CI 失败**：
CI 查的是 fmt / clippy / test 三样，而我只验了后两样的一半。

**注意：③ 为什么带 `CARGO_INCREMENTAL=0`**（2026-10-28 两次独立交付各踩一次）：

沙箱里的 shell 会在 `target/debug/incremental/mc_downloader-*/` 下留下**删不动也 rename 不动**的文件；
cargo 下一轮往里写 `dep-graph.part.bin` 时被拒 ⇒ `exit 101` + `error: could not compile ... os error 5`。
下一次跑还会撞同一堵墙（自锁），而且**每次 cargo 跑完都可能重新楔住**。

**判据：`TEST=101` 是工具自述，`error: could not compile` 才是产物 —— 红的是构建，不是测试。**
不看日志就会把它报成「代码改坏了」。`CARGO_INCREMENTAL=0` 不写增量 session 目录，从源头绕开；
「跑之前先删 `target/debug/incremental`」也行，但那个删除必须在**沙箱外**做，不如前者可靠。

### 测试策略：两类，分工不同

**现有 186 条 lib 单元测试 + 40 条集成测试（19 参数化 + 5 属性 + 8 HTTP 语义 + 5 Linux 语义 + 3 个回归文件），只覆盖「能想到的失败」。** 真实网络行为（丢包、RST、半关闭、
`Content-Length` 与实际不符）靠 **Kali 做中间人/服务端**、宿主侧配合测出来，
记录在 `docs/reports/net_faults-report-*.md`。

| 类型 | 作用 | 频率 |
|---|---|---|
| 单元测试（`cargo test --lib`） | **固化**已知问题，防回归 | 每次改代码 |
| Kali 环境测试 | **发现**未知失败模式 | 阶段性 / 大改后 |

**两类不能互替**：只做前者，测的永远是你想到的；只做后者，发现的东西会随环境消失。

**第三类产物：定量报告**（`docs/reports/quant-report-*.md` + `scripts/quant/`）—— 它不测"对不对"，测的是**默认值合不合理**：
并发 / 超时 / 分片 / 预分配这类数，没有实测就别当参数用。写这类报告的三条规矩（2026-11-22 立）：
① 口径**先写下来再测**，事后不改判据；② 每个数可回溯到 `raw CSV → 脚本 → 图/数`；
③ **没差异、证伪、没跑**都要写，那和"有收益"一样是交付（例：预分配就是被这样否掉的）。

**硬性要求：每次 Kali 测出的问题，必须在单元测试里补一条最小复现。**

理由是知识会忘 —— 不沉淀的话，下次改了分片逻辑还得重新起一次 VM 才知道坏没坏。
**判据**：`docs/reports/net_faults-report-*.md` 里每一条故障，都能在测试代码里指到对应的测试函数。

**参数化与属性测试已落地**（2026-11-22，#3）：`dev-dependencies` = `rstest 0.23` + `proptest 1`。

| 层 | 在哪 | 管什么 |
|---|---|---|
| 手写 `#[test]` | `src/**` 的 `mod tests` | 具体行为与错误分支（"这个输入必须得到这个结果"） |
| 参数化（rstest） | `tests/params_*.rs` | 同一断言的多组输入；**加边界只加一行 `#[case]`** |
| 属性（proptest） | `tests/props_*.rs` | **不变量**（"怎样都不该发生"），反例由生成器找并自动缩小 |

**写属性测试的三条纪律**（每条都是踩过才写的）：
- **不写死期望值** —— 片大小公式这类还在动的东西，写死数值等于每次微调都要改测试。
- **不引用实现里的常量** —— 拿实现自己的 `PAGE` 验证实现自己的不变量是循环论证；契约值在测试文件头**独立声明**。
- **关掉 `failure_persistence`** —— 默认失败会往仓库根写 `proptest-regressions/`，等于悄悄改了写范围。

**提交门**（本机状态，新克隆/换机器要装一次）：

```bash
git config core.hooksPath .githooks      # 装了之后，暂存有 .rs 就跑 cargo fmt --check
```

**CI**：`.github/workflows/ci.yml` 跑四个 job —— `check`（ubuntu：fmt → shell 语法 → clippy → test → release build）、`check-windows`（发布物是 `.exe`，Windows 专属分支只能在这跑）、`msrv`（`toolchain: 1.99` + `--locked`，让 `rust-version` 不是一句注释）、以及测试条数下限（`0 tests` 与 passed 同外观，只能靠数量守）。**推送后要真的看结果**（`gh run watch`），
不要把"push 成功"当成"CI 通过"。

---

## 四、目录结构（改哪看哪）

```
src/
  main.rs        子命令分发（vanilla / run / pack / fetch / probe）+ 配置解析
  config.rs      TOML 的 [[task]] 结构
  version.rs     Mojang version manifest / version.json 解析 → Vec<Job>
  download.rs    ★ 下载引擎：client / run / one_split / one_stream / batch_opts / header_content_length
  progress.rs    ★ 进度行格式化（含「总长未知」分支）+ 限速令牌桶
  scheduler.rs   Job 调度（并发 16、SourcePool 的 EWMA 选源）
  source.rs      多源池（ewma_bps + fail_streak）
  piece.rs       分片的切片与续传状态（.part.state）
  vanilla.rs     原版安装
  fabric.rs      Fabric 加载器
  forge.rs       Forge 加载器（含 installer --offline 调用）
  neoforge.rs    NeoForge 加载器（版本映射与 Forge 不同，见文件头）
  pack/mod.rs    整合包（Modrinth / CurseForge）
```

**★ 标记的两个文件是最容易改出问题的地方**（下载引擎与进度），动它们前先读 `.agents/notes/`。

---

## 五、八个必须知道的坑（全部有实测记录）

| 坑 | 判据 / 症状 |
|---|---|
| **`Content-Length` 可能没有**（`Transfer-Encoding: chunked` 合法） | **不要 `bail`**：`size: None` ⇒ 自动走 `one_stream`。**2026-10-27 修过一次**（`24eb5a2`），此前会拒绝一整类站点 |
| **`--offline` 的 installer 不要传代理参数** | 传了会**诱使它联网** ⇒ `SSLHandshakeException` 或卡死（CPU 零增量）。**`--offline` 的语义就是"不联网"** |
| **一个文件失败不该中断整批** | 批量场景一律用 `download::batch_opts()`（`continue_on_error = true` + 末尾汇总）|
| **`SourcePool` 不是"主源挂了才换"** | 它按 EWMA 实测吞吐**一开始就分配**。改选源逻辑前先读 `source.rs` 的注释 |
| **只有 `.part` 没有 `.part.state` 时必须从头下** | 稀疏文件的空洞读出来是 0，**宁可重下不拼坏文件** |
| **续传判据必须用同一个 `piece_conns`** | 片大小是 `size / conns` 的函数：`PieceState::load` 曾固定按 conns=4 重建判据，于是 `--piece-conns 8` 的续传永远判过期、**静默从头重下**（不报错）。现在走 `load_with_conns(part, size, opts.piece_conns)`（2026-11-22 修，#5） |
| **服务器对 Range 回 200 不是错误，是要降级** | 200 的 body 就是整份 ⇒ 该退回 `one_stream`，不要 `bail` 掉整批。`fetch_piece` 吐 `RangeUnsupported` 标记，`one_split` 收到后先 abort 余下 worker、再截断 `.part` 重下（残留竞态写在类型注释里）。416 / 5xx 仍然照常失败（2026-11-22，#6） |
| **落点路径可能来自别人，必须过 `pathsafe`** | 三处来源：URL 末段（**percent-decode 之后**才见真章）、整合包清单（`files[].path` / CurseForge `file_name`）、zip 条目名。一律走 `crate::pathsafe::{check_name, check_rel, under_root}`。**`PathBuf::join` 遇绝对路径会整体替换**（`join("C:\\x")` ⇒ `C:\x`），只查 `..` 挡不住 —— 2026-11-22 复现过越界写并修复，见 `implemented/bug-fix/2026-11-22-path-safety.md` |

---

## 六、决策记录在哪（别在代码注释里重写一遍）

**`.agents/notes/{lifecycle}/{class}/yyyy-mm-dd-topic.md`**

- **lifecycle**：`proposed` / `implemented` / `rejected` / `archived`
- **class**：`feature` / `bug-fix` / `simplification` / `architecture` / `process`
- **头三行必须恰好是**：`# Agent Note: <title>` + 空行 + `Status: ...`
- **正文**：`proposed` 用 `## Proposal` + `## Acceptance criteria` + `## Risks`；
  其余用 `## Problem` → `## Decision` → `## Alternatives considered`（**强制**）→ `## Consequences`
- **不建中央 INDEX**（索引会腐烂，用文件夹 + 搜索当索引）

**什么时候写**：代码/测试/本文件都承载不了的**持久决策理由**（为什么这样、放弃了什么）。
**什么时候不写**：机械改动、局部 UI 调整、纯重命名。

---

## 七、提交约定

- **提交信息**：中文，`type(scope): 一句话`，body 写「为什么」而不是「改了什么」。
- **别一次提交混两件不相关的事**。
- **文档不用 emoji**：任何 emoji 区段的字符都不写（勾叉、警告、暂停、星、文档图形符号等一律算），
  一律写文字（完成 / 未做 / 注意 / 暂停 / 重点 / 支持 / 不认）。箭头 → ⇒ ← 、圈码 ①②③ 、★ **不算** emoji，表格靠它们读，可以留。
- **改完代码要更新 README 的「验证状态」表**（那张表区分「结构对」和「真的能跑」，**不许含糊**）。
