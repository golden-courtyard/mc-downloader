# AGENTS.md —— mc-downloader

> **给改这个仓库的 AI 看**。用户向说明在 `README.md`；决策理由在 `.agents/notes/`。
> 三者分工：README 回答「怎么用」，本文件回答「改的时候不能踩什么」，notes 回答「为什么是现在这样」。

---

## 一、这个项目是什么（一句话）

把「Minecraft 相关文件放到正确位置」做成**可脚本化、可复现、可审计**的命令行工具。
**不做启动器**（启动/账号/存档交给 PCL、HMCL）。

---

## 二、铁律（改代码前必读，每条都有代价换来的理由）

| # | 铁律 | 为什么 |
|---|---|---|
| **1** | **不内置任何非官方镜像**，也不自动把 GitHub 换成第三方镜像 | 用哪个源由用户决定。`probe` 只负责测速，**不负责选源**。 |
| **2** | **对 CurseForge 标记「禁止第三方分发」的模组，只生成 `manual.txt`，不取直链** | 这是法律边界，不是技术限制。 |
| **3** | **不提供账号验证绕过 / 离线破解 / 任何"非官方"下载源** | 见 `README.md` 的法律定位一节。 |
| **4** | **不启动游戏、不管理账号** | 定位边界。加了就变成"启动器"，那是另一个产品。 |
| **5** | **SHA1 校验失败必须重下，不许"跳过校验继续"** | 校验是唯一的完整性保证；放过一次，之后所有"校验通过"都失去意义。 |

---

## 三、构建与验证（判据都是"跑出来的"，不看 IDE 脸色）

```bash
cargo fmt --all                                    # ① 格式（pre-commit 门会拦）
cargo clippy --all-targets --locked -- -D warnings  # ② lint（-D warnings = 警告即错）
cargo test --lib --locked                          # ③ 单测（当前 113 条）
cargo build --release                              # ④ 产物：target/release/mc-downloader.exe
```

**⚠️ 提交前必须跑完 ①②③**。这不是建议 —— **2026-10-25 曾因只跑「能编译 + 单测」而连续 5 次 CI 失败**：
CI 查的是 fmt / clippy / test 三样，而我只验了后两样的一半。

**提交门**（本机状态，新克隆/换机器要装一次）：

```bash
git config core.hooksPath .githooks      # 装了之后，暂存有 .rs 就跑 cargo fmt --check
```

**CI**：`.github/workflows/ci.yml` 跑 ①②③④。**推送后要真的看结果**（`gh run watch`），
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

## 五、五个必须知道的坑（全部有实测记录）

| 坑 | 判据 / 症状 |
|---|---|
| **`Content-Length` 可能没有**（`Transfer-Encoding: chunked` 合法） | **不要 `bail`**：`size: None` ⇒ 自动走 `one_stream`。**2026-10-27 修过一次**（`24eb5a2`），此前会拒绝一整类站点 |
| **`--offline` 的 installer 不要传代理参数** | 传了会**诱使它联网** ⇒ `SSLHandshakeException` 或卡死（CPU 零增量）。**`--offline` 的语义就是"不联网"** |
| **一个文件失败不该中断整批** | 批量场景一律用 `download::batch_opts()`（`continue_on_error = true` + 末尾汇总）|
| **`SourcePool` 不是"主源挂了才换"** | 它按 EWMA 实测吞吐**一开始就分配**。改选源逻辑前先读 `source.rs` 的注释 |
| **只有 `.part` 没有 `.part.state` 时必须从头下** | 稀疏文件的空洞读出来是 0，**宁可重下不拼坏文件** |

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
- **改完代码要更新 README 的「验证状态」表**（那张表区分「结构对」和「真的能跑」，**不许含糊**）。
