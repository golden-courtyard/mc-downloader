# Agent Note: 不可信的落点路径必须过 `pathsafe`（URL / 整合包清单 / zip 条目）

Status: implemented

## Problem

下载与安装的**落点**有三处来自别人，而落盘代码只做了 `root.join(path)`：

| 来源 | 位置 | 为什么不可信 |
|---|---|---|
| URL 末段（`fetch` / `urls_file`） | `url_list::filename_from_url` | percent-decode **之后**才见真章：`%2e%2e%2f%2e%2e%2fpwned.bin` 解出来是 `../../pwned.bin` |
| 整合包清单 | `mrpack::parse` 的 `files[].path`、`curseforge::split` 的 `file_name` | 清单是包作者写的；`../../evil.jar` 一样合法 JSON |
| zip 条目名 | `pack::extract_overrides` | `entry.name()` 是原始字符串，`overrides/../../evil` 照样 `create_dir_all` 再写 |

三处都汇到同一行 `root.join(&job.path)`。而 `PathBuf::join` 有个要命的性质：
**遇到绝对路径会整体替换**（`root.join("C:\\x")` 得到 `C:\x`）、UNC 同理 ——
所以"看着像相对路径"并不等于"落点还在 root 之下"，只查 `..` 是不够的。

实测复现（2026-11-22，`python -m http.server` 那种对任何路径都回 200 的源）：

```
fetch 'http://127.0.0.1:18111/loot/%2e%2e%2f%2e%2e%2fpwned.bin' --size 4096 --dir ...\out
[*] 落地 : ...\out\../../pwned.bin
[+] 完成 -> ...\out\../../pwned.bin       # ← 落在 --dir 两层之外，4096 字节，内容由服务器决定
```

`%2e%2e%5c%2e%2e%5c`（反斜杠变体）同样越界。配合"下载一个恶意 `.mrpack`"就是远程触发的任意路径写。

## Decision

新增 **`src/pathsafe.rs`**，判定只写一份，五个接缝都调它：

```rust
pub fn check_name(raw: &str) -> Result<String>;          // 单段文件名（URL 末段 / CF file_name）
pub fn check_rel(raw: &str)  -> Result<PathBuf>;         // 允许子目录（清单 / zip 条目）
pub fn under_root(root: &Path, rel: &Path) -> Result<PathBuf>;  // 落盘前的最后一道闸
```

拒绝规则：空/`.`/`..` 组件 · 绝对路径 · 盘符（`C:`、`C:foo`）· 控制字符（含 NUL）· 反斜杠；
**Windows 额外**：NTFS 备用数据流（`:`）· 尾部点或空格 · 设备保留名（`CON`/`NUL`/`COM1-9`/`LPT1-9`，
带扩展名也算）。

`under_root` 两层判定：**词法**（不依赖文件系统，`root` 还没建也成立）+ **符号链接**
（`root` 能被 `canonicalize` 时把两边最长可解析前缀再比一次；做不到时在注释里写明做不到，不假装做过）。

**政策分档**（这是本决策的核心，不是实现细节）：

| 场景 | 处置 | 理由 |
|---|---|---|
| 下载目标（`Job.path`） | **拒绝整条任务**，错误点名来源 | 静默改名会让"文件下到哪去了"变成谜，也会把攻击藏起来 |
| zip 条目 | **跳过并计数**，最后汇总一行警告 | 一个坏条目不该毁掉整次安装；但必须让人看见 |
| CurseForge `file_name` | 降级进 `manual.txt` | 与"禁止第三方分发"同一套人工处理路径 |
| URL 末段 | 回落 `out.bin` + 打一行提示 | 与"推不出文件名"的既有行为一致（两侧都是"这个 URL 给不出名字"）|

接缝：`download::one`（最后一道）、`url_list::filename_from_url`（解码之后）、
`mrpack::parse`（清单解析时即拒）、`curseforge::split`、`pack::extract_overrides`。

## Alternatives considered

| 方案 | 为什么不 |
|---|---|
| **只查 `..`** | 挡不住绝对路径与盘符 —— `join` 会整体替换。这是最容易写出来的假安全 |
| **一把梭 `canonicalize`** | 目标文件大多还不存在，`canonicalize` 直接失败；只能对"已存在前缀"做，所以必须配词法判定 |
| **只信 `zip` crate 的 `enclosed_name()`** | 它覆盖 zip 条目这条腿，但对 `C:` 之类只返回 `None`、不说明原因；也不管 `Job.path` 那条腿。留着它做第二意见可以，做唯一防线不行 |
| **静默改名（把 `..` 换成 `_`）** | 用户会拿到一个"名字不太对但下载成功"的文件，攻击被无声吞掉。宁可失败 |
| **沙箱 / 命名空间隔离** | 根治得动进程模型（Windows 上要 Job Object + 降权，Linux 要 namespace），是另一个量级的工程，且与本工具的"单次命令、跑完退出"形态冲突 |
| **`O_NOFOLLOW` / 全路径 fd 系（`openat`）** | 能根治 TOCTOU，但 Windows 没有对应语义，要写两套 —— 见 Consequences 的残留 |

## Consequences

**已修**：三处来源 + 落盘闸；单元 10 条（`pathsafe`）+ 3 条接缝测试 +
集成 `job_path_traversal_is_rejected_before_any_network_action`（用必连不上的端口证明"检查在请求之前"）。
手工复跑：两条穿越 URL 现在都被拒并回落 `out.bin`，`--dir` 之外零文件。

**代价（要知道，别以后当成 bug）**：
- `check_rel` **拒反斜杠**。Linux 上 `a\b` 是合法文件名，现在会被拒 —— 这是为了不让同一段代码在两个平台
  有不同语义；真遇到这种名字的用户会看到明确报错，不是静默改。
- Windows 专属规则（ADS、保留名、尾部点空格）**只在 Windows 生效**：在 Linux 上写 `a.txt:evil` 是普通文件，
  拿 Windows 的规则去拒它属于张冠李戴。

**已知残留**：**TOCTOU**。我们做的是"词法 + 现有祖先 canonicalize"，如果攻击者能在
`under_root` 校验通过之后、写入之前把某一层换成软链，仍然能逃出去。要根治得上 `openat2`/`O_NOFOLLOW`
或 Windows 的重解析点检查，两边语义不同、工作量不小；当前威胁模型（下载别人给的包）里，
这个窗口需要攻击者已经在机器上有并发写权限，收益不匹配。留作已知项，不假装没有。

**红队复核（2026-11-22，28 个用例）又挖出两处，已修**：

1. **`fetch --force` 是"先删、再拒"**：`fetch.rs` 在 `download::run` 之前就 `dir.join(name)` → `exists()` → `remove_file`，
   而 `pathsafe::under_root` 要到 `download::one` 才跑。于是 `--force -n "..\victim.txt"` 会**删掉 `--dir` 之外的文件**
   （复现两次，`c\victim.txt` 消失了）。`--name` 是用户输入、不是远端，所以不算远程穿越，
   但它违反"拒绝整条任务"的承诺。修法：把校验提到碰文件系统之前（`resolve_dst`），并给 `commands` 补了单测。
2. **`urls_file` 一行坏就整批拒**：`url_list::parse` 在第一条推不出名字时直接 `bail`，而 `batch_opts` 的
   "一个失败不拖累其余"只在下载阶段生效 ⇒ 混排列表里合法的那条一个字节都没下。修法：坏行（推不出名字 / 重名）
   跳过并计数，只有"一条有效都没有"才报错。

两处都是同一类错误：**把"检查"放在了"产生副作用"之后**。判据写在这里供以后复用：
凡是"先动文件系统、再校验输入"的代码路径，一律视为缺陷。

**仍未覆盖（红队指出的验证缺口）**：`pack` 的 overrides 闸**没有离线端到端路径** ——
`extract_overrides` 之前要先下 vanilla（`pack/mod.rs:43`），所以 `overrides//abs`、`overrides/C:/x`
只能靠单测（2026-11-22 补了两条）。要端到端验，需要 pack 的离线模式或可注入的下载器。

**顺带**：`AGENTS.md §五` 加了一条坑；`.agents/notes/implemented/architecture/2026-10-28-content-range-check.md`
里"落盘前不校验路径"的隐含假设就此失效 —— 那条 note 讲的是 `Content-Range`，路径安全由本条承担。
