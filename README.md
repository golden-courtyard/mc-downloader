# mc-downloader

> **个人工具，非通用下载器。** 为我自己的一套流程写的：可脚本化、结果可审计、失败能续。
> 不面向第三方用户，不做 GUI，不与 PCL / aria2 / IDM 抢通用场景；
> issue 只收我自己踩到的问题，讨论走 Discussions。

> 把 Minecraft 相关文件放到正确位置：下载结果就是标准 `.minecraft` 目录结构，可被 PCL / 官方启动器 / HMCL 识别。
> **下载源由你自己提供**（本工具不内置任何源，见下）。

> 如果这个方向对你有用，可以 [请我喝杯奶茶](https://afdian.com/a/liumiaoqi)。


> 注意：**使用前请读**：[免责声明与法律定位](#免责声明与法律定位)（文末）。
> **一句话**：只把你给的地址上的**公开文件**搬到你指定的目录；不包含、不分发任何游戏内容；不提供破解或账号绕过。

---

## 五分钟上手

### 0. 先配下载源（vanilla / run / pack 必需）

**本工具不内置任何下载源** —— 官方或第三方都不内置（理由见 [为什么源要你自己配](#为什么源要你自己配)）。
先在配置文件里填六项：

```toml
[source]
metadata    = "https://<你的源>/version_manifest_v2.json"
assets      = "https://<你的源>"
fabric      = "https://<你的源>"
forge_meta  = "https://<你的源>"
forge_maven = "https://<你的源>"
neoforge    = "https://<你的源>"
```

不想手写？`mc-downloader init` 会建好平台配置目录并写入带注释的模板（Windows：`%APPDATA%\mc-downloader\config.toml`；Linux/macOS：`~/.config/mc-downloader/config.toml`），再把六个键填上即可。

配置文件放哪：`--config <路径>`，或平台配置目录（`mc-downloader --print-config` 会告诉你它读的是哪个）。
**`fetch <url>` 不需要源** —— 你给地址，它只做传输。

不知道六个键各自该填到哪一层、官方端点在哪？见 [docs/user/sources.md](docs/user/sources.md)。

### 1. 下一个文件（最常用）

```bash
mc-downloader fetch <url> -d .
```

比裸 `curl` 多做四件事：大文件**自动分片并行**（≥8 MB）、**断点续传**（中断后重跑只补缺口）、
**失败指数退避重试**、**可限速**。服务器不给 `Content-Length`（如 `chunked`）时自动单流下到 EOF。

### 2. 装一个原版

```bash
mc-downloader vanilla 1.20.1 -d "D:/Games/.minecraft"
```

### 3. 原版 + 加载器（一步装好）

写一个 `a.toml`：

```toml
[[task]]
kind = "mc"
version = "1.20.1"
dir = "D:/Games/.minecraft"
loader = "fabric"          # 或 "forge" / "neoforge"；也可写 "forge:47.4.10" 指定版本
```

```bash
mc-downloader run a.toml
```

### 4. 装整合包

```bash
mc-downloader pack "xxx.mrpack" -d "D:/Games/.minecraft"
```

### 装完之后

**用 PCL / HMCL「添加游戏目录」指向 `-d` 的那个目录**，选对应版本启动。

> 注意：**本工具只保证目录结构**（`versions/<id>/<id>.json` + `libraries/`），
> **是否真能启动由你实际打开验证**。

### 出错了先看这三处

| 症状 | 先看 |
|---|---|
| 某个文件下不动 | 加 `--retry 5`；或 `probe` 测一下源速再填 `mirrors` |
| 加载器装完启动崩溃 | 看 `versions/<id>/<id>.json` 的 `mainClass` 是否有值 |
| 想确认配置写得对不对 | 先跑一遍，看它打印的「待下载 N 个文件」是否符合预期 |

---

## 文档地图

| 你想干什么 | 看哪里 |
|---|---|
| 装好它、把目录搞对 | 本文的「五分钟上手」 |
| 查参数/配置键的准确含义 | [docs/user/usage.md](docs/user/usage.md) |
| 改代码（构建、测试、提交门） | [CONTRIBUTING.md](CONTRIBUTING.md) |
| 改代码前不能踩什么 | [AGENTS.md](AGENTS.md) |
| 为什么是现在这样 | [.agents/notes/](.agents/notes/) |
| 架构与关键取舍 | [docs/design/architecture.md](docs/design/architecture.md) |
| 每个版本改了什么 | [CHANGELOG.md](CHANGELOG.md) |
| 全部文档的索引 | [docs/README.md](docs/README.md) |
## 是什么

一个**单文件命令行工具**，解决四件事：

| 场景 | 命令 |
|---|---|
| 我要某个 MC 版本的完整文件 | `vanilla` |
| 我要原版 + 加载器（Fabric / Forge），一步装好 | `run`（配置里写 `loader`）|
| 我有整合包（Modrinth / CurseForge），想铺成能启动的实例 | `pack` |
| 我要下单个文件（任意 URL，可分片 / 续传） | `fetch` |

**不做启动器**——启动、模组管理、存档、账号，交给 PCL 这类启动器。

### 验证状态（2026-11-23）

| 项 | 状态 |
|---|---|
| 1.20.1 + **Forge 47.4.10** → 目录结构 | 完成：与官方 installer 产物逐项一致（`mainClass` / patched client / libraries 数）|
| **1.20.1 + Forge 47.4.10 → 实际启动** | 完成：**用户用 PCL 实测，可进入游戏** |
| **1.20.1 + Fabric 0.19.5 → 实际启动** | 完成：**用户用 PCL 实测，可进入游戏** |
| CurseForge 整合包 188 个 mod 对账 | 完成：三个包全都对得上（`mods/` + `manual.txt` = manifest 条数）|
| **NeoForge 21.1.265（MC 1.21.1）** | **真机通过（2026-10-27）** —— `EXIT=0`·210s·`mainClass = cpw.mods.bootstraplauncher.BootstrapLauncher`·产物 `versions/neoforge-21.1.265/`。此前卡在下载阶段是**网络**，装配阶段还暴露过三处代码 bug（已修，见 `2f6d09d`）|
| **源码层：不内置任何下载源（#15）** | 完成（2026-11-23）：`src/` 里 11 处硬编码源全部删除，改为 `[source]` 六项**全必填**；CI 加 `grep` 闸门（实测零命中）；`vanilla` 未配源时打印「填哪个文件、缺哪个键、只想下单个文件就用 `fetch`」 |
| **落地名认 `Content-Disposition`（#16）** | 完成（2026-11-23）：优先级 `--name` > CD > URL 末段 > `out.bin`；服务器给的名字同样过 `pathsafe`（`filename="../../evil.bin"` 实测回落且目录外无文件） |
| **远端对象中途被换（#7）** | 完成（2026-11-23）：416 报「远端当前 N 字节，我们按 M 字节在切」；`--if-range` 打开时条件失配 ⇒ 当场降级单流重下（32 MiB 实测 sha1 一致） |
| **协议层五项（#10）** | 完成（2026-11-23）：未经请求的压缩 ⇒ 拒绝且未落盘；`--require-sha1`；302 换对象无跨对象拼接；slowloris 实测曾挂 7 分钟 ⇒ 新增停滞检测后 10.0 s 内失败，4 MiB 正常下载无误杀 |
| **分片写入页对齐（#5）** | 注意：**结构对 + 本机实下**：单测断言片起点与片长都是 4096 的倍数；96 MiB / `--piece-conns 7` 实下输出「8 片 · 片大小 14376960 字节」（= 3510×4096），成品与 `curl` 独立取回 **SHA1 一致**。真机 I/O 证据（改前 8 个写偏移里 7 个不对齐 → 改后 8/8 对齐）见 `docs/reports/quant-report-20261122.md` 的 E3 |

> 注意：这张表**区分「结构对」和「真的能跑」**。只有标「实测」的那条是人工确认过的；
> 其余是文件级比对 —— **结构一致不等于能启动**。

---

## 用法
### 几个值得先知道的参数

| 参数 | 作用 |
|---|---|
| `--print-config` | 打印**生效值**和它来自哪一层（CLI / 配置文件 / 默认），排查"我改了怎么没用" |
| `--config <路径>` | 指定配置文件；不给则按平台配置目录找（`--print-config` 会告诉你读的是哪个） |
| `--conns` / `--jobs` | `--piece-conns` / `--concurrency` 的别名 |
| `--if-range` | 分片带 `If-Range`：远端中途被换时**当场降级重下**（默认关，见 `docs/user/usage.md`） |
| `--require-sha1` | 没有校验和的文件**拒绝下载**（默认关） |
| `--no-content-disposition` | 不读服务器给的文件名，严格按 URL 决定落点 |
| `--no-motw` | 不给 `.exe`/`.dll` 打 Mark-of-the-Web（Windows；默认会打） |

> **用于下载你有权下载的文件。** 本工具不判断使用者是否持有相应授权，这一点由使用者自行负责。

**完整教程见 [docs/user/usage.md](docs/user/usage.md)** —— 覆盖 `fetch`（下一个文件）、`run`（批量 / 脚本编排）、`probe`（先测再选源），以及六个调优参数（`--concurrency` / `--piece-conns` / `--adaptive` / `--retry` / `--timeout` / `--max-speed`）的实际取舍。下面只给最短的路径。

### 下载原版

```bash
mc-downloader vanilla 26.3 -d "D:/Games/.minecraft"
```

做：

```
versions/26.3/26.3.json      版本描述（启动器靠它识别版本）
versions/26.3/26.3.jar       客户端
libraries/...                依赖库（含 natives-windows）
assets/indexes + objects     资源
```

### 安装整合包

```bash
mc-downloader pack "xxx.mrpack" -d "D:/Games/.minecraft"
```

做：

```
① 读 modrinth.index.json → 取 minecraft / fabric-loader 版本
② vanilla::install 下原版
③ fabric::install 拉 Fabric profile（落盘 + 下 libraries）
④ 按 files[] 下模组（SHA1 校验，跳过 env.client == unsupported）
⑤ 复制 overrides / client-overrides 到实例目录
```

产物是 `versions/fabric-loader-<loader>-<mc>/`，PCL「添加游戏目录」指向 `-d` 的那个目录即可启动。

---

### 探测多个源的速度（选 mirrors 用）

```bash
mc-downloader probe <url1> <url2> [...] --seconds 5
```

**输出按吞吐降序排名**，把快的填进 `mirrors`：

```toml
[[task]]
kind = "generic"
url = "https://github.com/.../big.zip"        # 主 URL
mirrors = ["https://mirror.example.com/big.zip"]   # 内容相同的其它源
dir = "D:/Downloads"
```

**下载时 `SourcePool` 会按实测吞吐（EWMA）把分片分配给各源** —— 不是"主源挂了才用镜像"，
而是**一开始就按快慢分配**。

**实测差距可以很大**（同一台机器、同一代理）：

```
GitHub release :   25–92 KB/s
清华 TUNA      : 10.9–35 MB/s     ← 差 400 倍左右
```

⇒ **先 `probe` 一下再填 `mirrors`，比盲目加镜像有用得多。**

> 注意：**本工具不内置任何非官方镜像**，也不自动把 GitHub 换成第三方镜像 ——
> 用哪个源由你决定。`probe` 只负责测，不负责选。

---

## 支持范围

| 输入 | 支持 |
|---|---|
| 原版任意版本 | 支持 |
| **Modrinth 整合包**（`modrinth.index.json`）| 支持 |
| **CurseForge 整合包**（`manifest.json`）| 支持，需 `CURSEFORGE_API_KEY`；禁分发的模组生成 `manual.txt` |
| 结构相同的 `.zip`（只改了扩展名）| 支持 |
| **Fabric** 加载器 | 支持，**1.20.1 已实测可启动** |
| **Forge** 加载器（`loader = "forge"` / `"forge:47.4.10"`）| 支持，需 Java；下载由本工具做，processor 交给官方 installer。**1.20.1 已实测可启动** |
| **NeoForge** 加载器（`loader = "neoforge"` / `"neoforge:21.1.264"`）| 支持，需 Java 21+；版本号规则与 Forge 不同（MC `1.21.1` → NeoForge `21.1.x`），**NeoForge 从 MC 1.20.2 开始**，1.20.1 及更早请用 forge |
| **HMCL / MCBBS / MMC** | 不认 |

> 不支持不是"做不了"，是**没做**。加解析器即可，但会先确认需求。

> 注意：**加载器只保证目录结构**（`versions/<id>/<id>.json` + `libraries/`）。
> **是否真能启动，本工具不验证**——请用 PCL / HMCL 这类启动器实际打开一次。

---

## 特性

### 下载

- **并发 16 个文件**（`buffer_unordered`），tokio 多线程运行时。
  `run --parallel` 可让多个 `[[task]]` 并行，总并发会被均分（`split_for`），不会 N×16
- **单文件分片**：≥ 8 MB 的文件切分片、`piece_conns` 个连接并行（默认 4）
- **断点续传**：`.part` + `.part.state` 记录哪些片段已完成，重跑只补缺口。
  **只有 `.part` 没有状态文件时从头下**（稀疏文件的空洞读出来是 0，宁可重下不拼坏文件）
- **逐文件 SHA1 校验**：Modrinth / 原版 manifest / CurseForge 都提供哈希；
  内容被篡改一个字节会被发现并重下（用变异测试验过）
- **多源按实测吞吐分配**：`mirrors` 给多个源，`SourcePool` 用 EWMA（`ewma_bps` + `fail_streak`）
  挑最快的分片 —— **不是"主源挂了才换"**
- **全局限速**：`--max-speed 5M`，令牌桶，限的是**总带宽**而非每连接
- **批量 URL 列表**：`urls_file = "list.txt"`，对齐 aria2 的 `-i` 格式
  （选项行会跳过并计数）；**一个失败不中断**，最后统一报失败项 + 退出码非 0

### 平台

- **平台过滤**：`libraries` 按 `rules` 只下当前平台的，native 走 `classifiers["natives-windows*"]`

### 其他

- **源速度探测**：`probe <url1> <url2> ...` 串行各测 N 秒，报吞吐排名（填 `mirrors` 前先探）
- **纯本地**：无账号、无遥测、无网络请求以外的副作用

---

## 构建与开发

```bash
cargo build --release        # 产物: target/release/mc-downloader.exe（需 Rust 1.99+）
```

**改代码前请读 [`AGENTS.md`](AGENTS.md)** —— 铁律、构建验证三件套（fmt / clippy / test）、
八个必须知道的坑、以及决策记录该写在哪。

---

## 为什么源要你自己配

本工具**不内置任何下载源** —— 官方地址、第三方镜像都不内置，也没有"试试这几个"的探测。

理由是 2026-10-20 那天：PCL2 在 v2.99.0.0 移除了 MC 下载功能，更新日志原文写的是
「为遵守相关法律法规」。原因不是被举报，而是「第三方工具内置下载功能客观上存在绕过官方正版验证的可能」。
我们和它相邻 —— 与其等别人来指出，不如把这条通道交还给你：

- 你要下什么、从哪下，由**你**决定并承担；
- 本工具只负责"把文件放对位置、可续传、可校验、可脚本化"；
- 换镜像 = 改配置，不用等我们发版。

判据不是口头承诺：`src/` 里出现任何源地址，CI 会直接红（`grep` 闸门）。
`docs/user/usage.md` 的「源：必须先自己配」写了六个键各填什么。
## 为什么不用启动器的下载功能

> **"把文件放到正确的位置"这一件事**，做成一个可脚本化、可复现、可审计的命令。

不替代启动器，也不依赖启动器的内部实现。**完整理由见 `.agents/notes/`。**

---

## 免责声明与法律定位

**本工具属于通用下载工具，具有实质性非侵权用途。**

它只做三件事：

1. 从**你在 `[source]` 里配置的地址**下载 Minecraft 的**公开可访问**文件（原版清单、assets、加载器 profile/库）；
2. 从整合包 manifest 里列出的**作者公开提供的 URL** 下载模组（逐文件 SHA1 校验）；
3. 把上述文件按标准 `.minecraft` 目录结构落盘。

**它不自带任何下载地址**：填哪个元数据/资源/加载器源由你决定，工具只负责传输与装配。

**它不做的**：

- 不包含、不分发任何游戏内容
- 不提供账号验证绕过、离线破解
- 不提供任何"非官方"下载源
- 不修改任何第三方软件
- 不针对特定侵权用途设计任何功能

### 相关法律条文（陈述条文本身，不构成本项目对自身合法性的判定）

| 依据 | 要点 |
|---|---|
| 《民法典》第 1197 条 | 网络服务提供者**知道或应当知道**侵权、**未采取必要措施**的，承担连带责任 |
| 法释〔2020〕19 号 第 7 条 | **明知或应知**侵权仍提供技术支持等帮助的，构成**帮助侵权** |

### 本项目的客观事实

以下只陈述可核验的事实，**不附法律结论**：

- **下载来源**：**全部由用户在配置的 `[source]` 段提供**（六个键），本工具不内置任何地址。
  整合包里模组的直链来自整合包 manifest 作者填写的内容，逐文件 SHA1 校验。
  唯一固定访问的外部服务是 CurseForge API（`api.curseforge.com`，取整合包元数据）；
  Forge 下载的库另有**基于 `[source]` 推出的域名白名单**，不在名单内的会被拒绝。
- **不包含、不分发任何游戏内容**；
- **不提供账号验证绕过或离线破解**；
- **不启动游戏、不管理账号**——启动交给 PCL / HMCL 这类启动器；
- **对 CurseForge 标记为禁止第三方分发的模组，只生成人工下载清单**（`manual.txt`），不取直链。

**是否构成侵权，由使用者、权利人及司法机关依法认定。本项目不作此判定。**

**使用前提**：使用者应自行确保对下载内容拥有合法授权。因使用本工具产生的任何法律责任，由使用者自行承担。

本项目与 **Mojang / Microsoft / PCL / HMCL / Modrinth / CurseForge 均无关联**。

---

---

## License

本仓是混合内容，**按类分开授权**：

| 内容 | 许可 | 说明 |
|---|---|---|
| 代码（Rust / R / Wolfram / Lean） | **MIT** | 见 [LICENSE](LICENSE) |
| 原创文档（`docs/`、`.agents/notes/`、`proofs/` 的说明文字） | **CC BY-NC-4.0** | 可读、可引用、须署名，**不得商用**；全文见 [LICENSE-CC-BY-NC-4.0](LICENSE-CC-BY-NC-4.0)，摘要见 [docs/LICENSE](docs/LICENSE) |
| 第三方材料 | **不随本仓授权** | 引用片段归原权利人，清单与出处见 [THIRD_PARTY.md](THIRD_PARTY.md) |

第三方材料的规则：只作研究与引用，不整篇转载；被引用的论文、标准、RFC
按各自条款处理，本仓不替它们授权。

---

## 工具链

代码、测试与文档在 [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) 上以 AI 协作方式完成；
决策理由记在 `.agents/notes/`，实验数据记在 `docs/reports/`。

四条结论（4 KiB 对齐 / 退避上界 / AIMD 平均 75% / 片数与并发数契约）在 [proofs/](proofs/) 里用 Lean 形式化，`lake build` 十秒可查；这些是**关于模型的命题**，不是实测结论。需要 `ring` / 分析学的写法见 [proofs/MATHLIB.md](proofs/MATHLIB.md)（本机 Mathlib 已编好，按文件选用）。

[![](https://img.shields.io/badge/powered_by-dsh-4D6BFE?style=flat-square&logo=deepseek&logoColor=white)](https://github.com/deepseek-ai/deepseek-harness)

如果这个方向对你有用，可以 [请我喝杯奶茶](https://afdian.com/a/liumiaoqi)。

- 社区友链 · [LINUX DO](https://linux.do/)
