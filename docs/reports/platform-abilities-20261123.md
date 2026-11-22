# 平台能力清单：值得用、但没用上的（2026-11-23）

> 问题起源：`verify` 子命令（issue #11）要先回答"本机到底有什么可调"；顺手把 Windows / Linux / HTTP
> 三侧同类问题一次查完，免得每个功能各自猜一遍。
>
> 口径：每条四栏 —— **能力 / 来源 / 我们现状 / 值不值（判据 + 代价）**。
> 判据写成"什么条件下才做"，代价写成"要付什么"；不满足判据就写"不值，因为…"，不给中间态。
> 二手博客不当事实：凡只有第三方站点说、没有官方口径或本机实测的，都标 **未核实、不采用**。
>
> 实测时刻：2026-11-22 21:00–23:30（文件名按 11-23 归档）。

## 0 本机环境（下面所有"本机实测"都指这一台）

| 项 | 值 | 取法 |
|---|---|---|
| OS | Windows 11 家庭版，build **26300**，AMD64 | `(Get-CimInstance Win32_OperatingSystem).BuildNumber` |
| Rust | rustc / cargo **1.100.0** | `rustc --version` |
| PowerShell | **7.6.10** | `$PSVersionTable.PSVersion` |
| 长路径策略 | `LongPathsEnabled = 1` | `Get-ItemProperty HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem` |
| 磁盘 | 0 = `SAMSUNG MZVL81T0HFLB-00BH1` NVMe SSD（C:）；2 = `Seagate BUP Slim` USB（D:）；1 = `Kingston XS1000` USB（E:） | `Get-PhysicalDisk` / `Get-Partition` |
| 可用空间 | C: 168.0 GB / D: 260.7 GB（2026-11-22 采） | `Get-PSDrive C,D` |

## A 火绒（本机实测 + 官方论坛）

### A.0 本机事实

| 探测 | 结果 | 命令 |
|---|---|---|
| 安装目录 | `C:\Program Files\Huorong\Sysdiag\bin`（**26 个 exe**；**没有** `HRSafe` 这个目录） | `Test-Path 'C:\Program Files\Huorong\HRSafe'` ⇒ `False`；`(Get-ChildItem … -Filter *.exe).Count` ⇒ 26 |
| 主程序版本 | `HipsMain.exe` FileVersion **6,0,0,1** / ProductVersion 6,0,0,0；文件描述「火绒安全软件 主程序模块」 | `(Get-Item …\HipsMain.exe).VersionInfo` |
| 签名 | **Valid**，签发者「北京火绒网络科技有限公司」（Sectigo EV，2024-02-20 → 2027-02-20） | `Get-AuthenticodeSignature` |
| 服务 | `HipsDaemon` **Running** / StartType Automatic（显示名 Huorong Internet Security Daemon） | `Get-Service HipsDaemon` |
| `root\SecurityCenter2` 注册的杀软 | **只有** `Windows Defender`（`productState=393472`，`pathToSignedProductExe=windowsdefender://`） | `Get-CimInstance -Namespace root/SecurityCenter2 -ClassName AntiVirusProduct` |
| Defender 自身状态 | `AMServiceEnabled=False`、`RealTimeProtectionEnabled=False`、`AntivirusEnabled=False` | `Get-MpComputerStatus` |

**这两行合起来就是 A 节最重要的结论**：这台机器上 **SecurityCenter2 说"有 Defender"，实际 Defender 是关的；
真正在跑的火绒不在 SecurityCenter2 里**。所以任何"查 SecurityCenter2 判断有没有杀软"的代码，
在本机给出的是双重错误答案（既漏了火绒，又把一个停用的 Defender 当成可用引擎）。
这不是本机怪癖：安全中心只登记"向它注册的"产品，HIPS 类软件（火绒个人版）本来就不注册。

另注：官方回复里的示例路径是 `C:\Program Files (x86)\Huorong\…`（32 位安装），本机是 `C:\Program Files\Huorong\…`。
拼命令行时别写死，按 `HipsMain.exe` 的实际位置取。

### A.1 能用的一条：`HipsMain.exe -s "<路径>"`（只拉起自定义查杀）

- **来源**（官方论坛，运营专员回复）：
  - [tid-148223](https://bbs.huorong.cn/archiver/?tid-148223.html)（2024-12-30）：
    「火绒安装目录\sysdiag\bin\HipsMain.exe，命令行参数：`-s "指定目录"` …… **命令行仅支持自定义查杀**」；
    2025-01-01 追问后回复「目前暂无其他命令查杀参数了」。
  - [tid-115491](https://bbs.huorong.cn/thread-115491-1-102.html)（2022-12）：「只有自定义查杀支持命令行
    `HipsMain.exe -s [file]`，而且**会启动 UI 进行，没有命令行当中的结果输出**」。
- **我们现状**：完全没用。`src/main.rs` 没有 `verify` 子命令（`Select-String -Path src\main.rs -Pattern verify` 无命中），
  下载路径也不调任何外部程序。
- **值不值**：**值，但只值"把扫描拉起来给人看"。**
  - 判据：用户愿意在扫描结束后自己点开火绒界面看结果；命令只负责"不用手点两层菜单"。
  - 代价：会弹 UI（不是静默）；没有结果输出（见 A.2）；**退出码的含义未实测**，不能拿它当判据。
  - **结论一句话：能把扫描拉起来给人看，不能当 gate。**

### A.2 拿不到的一条：扫描结果 / 安全日志

- **来源**（官方论坛）：
  - [tid-107476](https://bbs.huorong.cn/forum.php?mod=viewthread&tid=107476&extra=&ordertype=1)：
    「火绒的自定义查杀指定目录文件命令行参数为 `-s[File]`，**目前不支持打印安全日志的参数、指令**」。
  - [tid-132301](https://bbs.huorong.cn/forum.php?mod=viewthread&action=printable&tid=132301)（2023-10-07，问
    `HipsLog.exe` 有无参数）：「**不支持命令行导出日志**，建议手动导出安全日志」。
- **我们现状**：没有任何读取火绒日志的代码；`src/` 里也没有 `log.db` / `ProgramData\Huorong` 相关路径。
- **值不值**：**不值（做不到）。** 判据若写成"能拿到结论"，永远不满足 —— 官方两次明确拒绝，
  且 `HipsLog.exe` 没有参数。代价为零（不做）。
- 因此任何 `verify --huorong` 的输出**必须**自己说明这一点，见 A.5。

### A.3 `sysclean.exe` 是垃圾清理，不是杀毒

- **来源**：[tid-133595](https://bbs.huorong.cn/archiver/?tid-133595.html)（2023-11-16，运营专员）：
  「我们目前支持**命令行启动垃圾清理**，`C:\Program Files (x86)\Huorong\Sysdiag\bin` 路径打开 cmd，
  输入 `sysclean.exe` 回车」。
- **本机**：`sysclean.exe` 确实在 `…\Sysdiag\bin`（1136800 字节）。
- **我们现状**：没用（也不该用）。
- **值不值**：**不值。** 判据：与下载完整性有关才要；垃圾清理与"文件对不对"无关，误清理还会删掉用户文件。

### A.4 流传的 `HrSafe.exe /scan /full /deep`：未核实、不采用

- **来源（第三方，非官方）**：`mizhijia.net` 等站点 2026-04 的教程声称
  `"C:\Program Files\Huorong\HRSafe\HrSafe.exe" /scan /full /deep` 可"静默全盘深度扫描"。
- **本机反驳**：`Test-Path 'C:\Program Files\Huorong\HRSafe\HrSafe.exe'` ⇒ `False`；
  `Get-ChildItem 'C:\Program Files\Huorong'` 只有 `Sysdiag` 一个子目录。
- **官方口径反驳**：A.1/A.2 两条都在说"只有 `-s`、无其它参数"，没有任何 `/scan /full /deep`。
- **结论**：**未核实、不采用**。这类页面像是按关键词拼出来的（同一段文字在多个站点重复），
  不能当证据；把它写进代码会让功能在真装火绒的机器上静默失败。

### A.5 若做 `verify --huorong`，输出必须这么写

事实约束（A.1 + A.2）：能拉起、拿不到结果、退出码含义未实测。所以输出要**同时**给出"我做了什么"和"我拿不到什么"：

```text
已启动火绒自定义查杀：HipsMain.exe -s "<路径>"
注意：火绒个人版不提供命令行扫描结果或日志（官方：不支持打印安全日志的参数、指令），
      退出码含义也未实测、不作为判据；扫描结论请到火绒界面查看。
      这一步只负责把扫描拉起来，不是校验，也不能当 gate。
```

三条措辞上的硬要求（对应 issue #11 的判据 3）：

1. 不许写"已扫描""通过""安全"——本工具**不知道**结果；
2. 必须写"结果在火绒界面里，本工具拿不到"；
3. 与哈希/签名结论**分开列**，不许合并成一句"这个文件没问题"。

## B Windows

| 能力 | 来源 | 我们现状 | 值不值（判据 + 代价） |
|---|---|---|---|
| **MotW**（`Zone.Identifier` 备用数据流） | MS 文档：浏览器下载会写 `[ZoneTransfer] ZoneId=3`，之后 SmartScreen / 受保护视图才生效 | **正在做（未提交）**：`src/motw.rs`（`?? src/motw.rs`），`download.rs:325` 调 `mark_if_enabled`，`--no-motw`（`main.rs:56`）关闭，默认 `true`（`download.rs:77`） | **值（本轮已实现，待提交）**。判据：下载 `xxx.exe` 后 `Get-Item -Stream Zone.Identifier` 能读出 `ZoneId=3`；代价：只对 12 类可执行后缀各写一次 ADS（`motw.rs:17`），失败只告警不中断（`motw.rs:41`） |
| **`Get-AuthenticodeSignature`**（exe/dll 签名与链） | PowerShell 自带；本机实测 `HipsMain.exe` ⇒ `Valid` + 签发者 CN | **未用**：无 `verify` 子命令；`src/` 里没有签名校验（`Select-String uthenticode` 无命中） | **只值"提示"，不值"gate"**。判据：能把"谁签的/链是否有效"打出来；代价：Rust 侧要 WinTrust FFI（与 Cargo.toml 的 `unsafe_code = "forbid"` 冲突）或 shell 出 PowerShell（每文件一次进程）。**不能当 gate**：attack-report §2.4 的样本就是"合法签名 exe + 同目录恶意 dll" |
| **`GetDiskFreeSpaceEx` 预检** | Win32 API；`Get-PSDrive` 是其上层 | **完全没用**：`src/` 与 `tests/` 里 `free_space` / `available_space` / `ENOSPC` 均 0 命中；盘满只会在写第 N 个字节时炸 | **值**。判据：目标盘可用 < `job.size` 时，在写第一个字节前报「磁盘空间不足：需要 X，可用 Y」；代价：一次调用（Rust 侧可走 `windows-sys` 或 `std::fs` 无等价物 ⇒ FFI 问题同上一行），并要选对盘（`.part` 与目标同目录） |
| **`IOCTL_STORAGE_QUERY_PROPERTY`（seek penalty）判机械盘** | Win32 `DEVICE_SEEK_PENALTY_DESCRIPTOR.IncursSeekPenalty` | **未用**（`rotational` 0 命中）。用途本应是 issue #8（写局部性）的输入开关 | **不值（自动探测）**。本机反证：`Get-PhysicalDisk` 对两块 USB 盘（D:/E:）`MediaType=Unspecified`、`SpindleSpeed=0`，只有 NVMe 报 `SSD` ⇒ 自动判定在 3 块盘里错 2 块。判据改成"给 `--write-order` 显式开关"即可；#8 的收益已实测（E2：`WA` 1.345–1.718、墙钟 1.60–2.06×），不需要盘类型探测来撑 |
| **`PowerCreateRequest` / `SetThreadExecutionState` 防睡眠** | Win32 | **未用**（`ExecutionState` / `PowerCreateRequest` 0 命中） | **不值（现在）**。判据：单次任务时长 > 系统睡眠超时（默认 AC 15 min 量级）**且**有"跑一半被挂起"的复现。本机量级：1 GiB / 16 连在 D: 机械盘 **44.01 s**（quant E6 同臂），按 44 s/GiB 线性推算 10 GiB 约 7 min；E4 的 **17.96 min** 是 8 mbit 限速实验（1 GiB @ 8 mbit，`quant-report` §6），不是常规负载。代价：FFI（同上，`unsafe_code = "forbid"`） |
| **MAX_PATH > 260** | Windows 长路径策略 + `\\?\` 前缀 | **未特殊处理**（`MAX_PATH` / `\\\\?\\` 0 命中；`pathsafe` 只做前缀比对） | **不值（本机）**。实测：`LongPathsEnabled=1`，用 rustc 1.100 编的 15 行程序（`std::fs::write`）**直接写成 324 字符路径**、不带 `\\?\`，`dir /s /b` 能看到文件 ⇒ Rust std 已处理。代价：自己加前缀反而会破坏 `pathsafe` 的 `canonicalize` 前缀比对。边界：策略关闭的机器仍会失败（未测，理论上限 ~260） |
| **OneDrive 占位文件** | `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS` / 占位重解析点 | **未处理**（`OneDrive` / `RecallOnDataAccess` / `placeholder` 0 命中）；`C:\Users\<you>\OneDrive` 存在 | **未测，先文档警告**。风险点具体：`sha1_file` 会把整文件读一遍，占位文件会触发云端拉取（慢且费流量）；判据：在 Onedrive 目录里对一个占位文件跑 `fetch`，若 sha1 阶段出现"读很慢/网络流量"，就得处理。代价：读属性要 FFI |

## C Linux

> 本节的"本机"指 Kali VM（`/sys/block` 只有 `sda`，根是 ext4）。Windows 侧的写放大数据来自 quant 报告，不是 Linux。

| 能力 | 来源 | 我们现状 | 值不值（判据 + 代价） |
|---|---|---|---|
| **`posix_fadvise(POSIX_FADV_DONTNEED)`** | `man 2 posix_fadvise` | **未用**（`fadvise` 0 命中）。当前唯一的"大读"是 `sha1_file` 顺序读一遍 | **不值**。判据：存在"读完就不再需要、且会挤掉缓存"的读路径；sha1 只读一次即结束，丢缓存没有对象可救。代价：libc 一行，但收益不可测 |
| **`fallocate`** | `man 2 fallocate` | **未用**；现在用 `set_len` 稀疏预分配（`download.rs:424`，`if len != size { set_len }`）。E6 实测：**写零**预分配把 `WA` 1.718 压到 1.02，但端到端 44.01 s → **57.5 s（+31%）** ⇒ "先写一遍零"这条已被证伪 | **未测，暂不做**。`fallocate` 分配的是 unwritten extent，**不写零**，所以 E6 不能直接外推；判据：ext4 上先用 `iostat`/`filefrag` 证明 `WA > 1.2`，再测 fallocate 臂端到端 ≤ ±5%。代价：libc + 只有 Linux 生效，Windows 侧无对等物（NTFS 的稀疏 `set_len` 就是现在这条） |
| **`copy_file_range`** | `man 2 copy_file_range` | **未用且没有调用点**：全仓 `fs::copy` / `copy(` 0 命中（整合包 overrides 是解压写字节，不是复制大文件） | **不值**。判据：存在"复制大文件"的路径才谈得上；没有场景就是零收益。代价：无（不做） |
| **`statvfs`** | `man 3 statvfs` | **未用**（`statvfs` 0 命中） | **值**（与 B 节 `GetDiskFreeSpaceEx` 同一条判据、同一段逻辑，只是换实现）。判据：可用 < `job.size` 时提前报错。代价：libc 一次调用 |
| **`/sys/block/*/queue/rotational`** | Linux sysfs | **未用** | **值低、且不可信**。Kali 实测：`/sys/block/sda/queue/rotational` = **1**，但那是 virtio 虚拟盘 —— 虚拟盘/云盘/RAID 上这个值不代表物理介质。判据：只在"真机 + 失败就回退默认策略"下用；与 B 节的 seek penalty 合成一条读盘类型的旁路，别单独当判据 |

## D HTTP 侧

| 能力 | 来源 | 我们现状 | 值不值（判据 + 代价） |
|---|---|---|---|
| **`Retry-After`** | RFC 9110 §10.2.3 | **部分**：CurseForge API 已遵守（`curseforge.rs:463`，`backoff_ms` 把提示夹在 200–10000 ms）；**下载路径没有**，退避是纯 `1 << attempt`（`download.rs:280` 单文件、`download.rs:545` 分片），即 1s/2s/4s | **值**。判据：429/503 且带 `Retry-After` 时，总等待落在"服务端提示"而不是盲退的 1+2+4 s 上（能直接少一次被封）。代价：复用 `backoff_ms` 现成的 clamp，约 20 行 |
| **429 / 503 的归因** | RFC 9110；本机代码路径 | **会把它们说错**：分片路径把所有非 206/200/416 的状态都报成「服务器不支持 Range（HTTP {status}），无法分片」（`download.rs:907`） | **值（改文案 + 分支）**。判据：429/503/5xx 必须报「源站限流/暂时不可用」，408/其它才报协议问题 —— 现在的文案会把排查引到"服务器不支持 Range"这个相反方向。代价：一个 `match` 分支 + 一条错误信息 |
| **`If-Range` / `If-Match`（#7B）** | RFC 9110 §13.1.5 / §13.1.1；issue #7 | **#7A 已落地**：`check_content_range(..., Some(size))` 比对了 `Content-Range` 总长，错误是「远端文件大小变了」（`download.rs:807`，测试 `content_range_checks_total_against_expected_size`）。**#7B 未做**：`.part.state` 不存 validator，`PieceJob` 没有 ETag 字段 | **值（但排 #8 之后）**。判据：首片取 `ETag`/`Last-Modified`，后续分片带 `If-Match`/`If-Range`，对象被换时得到 412/200 而不是最后才 SHA1 失败。代价：`.part.state` 格式变更（旧状态必须作废而不是误读，照 `PAGE` 那次的规矩）+ 服务器不一定给 ETag（要给回退路径） |

## 结论：现在做 / 以后做 / 不做

| 判定 | 项 | 理由（一句话） |
|---|---|---|
| **现在做** | D 的 `Retry-After` + 429/503 归因 | 已有 `backoff_ms` 可复用；错误文案现在**是错的** |
| **现在做** | B 的磁盘空间预检（+ C 的 `statvfs`） | 一次调用换一个可读错误，避免写一半 |
| **本轮已实现（待提交）** | B 的 MotW | `src/motw.rs` 已在（未提交） |
| **以后做** | D 的 `If-Range`/`If-Match`（#7B） | 要与 `.part.state` 格式变更一起做 |
| **以后做（有判据）** | C 的 `fallocate` | 先证明 Linux 侧 `WA > 1.2`，否则不做 |
| **不做** | 火绒当 gate、读火绒日志、`HrSafe` | A.1–A.4：做不到，或来源不可信 |
| **不做** | Authenticode 当 gate | 白加黑（attack-report §2.4） |
| **不做（自动探测）** | 机械盘判定、防睡眠 | USB 桥接判不出来；睡眠判据要先有复现，且都要 FFI |
| **不做** | `copy_file_range`、`posix_fadvise` | 没有对应场景 |
| **未测（先记着）** | OneDrive 占位文件 | 需要真占位文件才能测 |

## 复现命令

```powershell
# A.0 本机火绒事实（只读）
Get-Service HipsDaemon | Format-List Name,Status,StartType
(Get-Item 'C:\Program Files\Huorong\Sysdiag\bin\HipsMain.exe').VersionInfo.FileVersion
Get-AuthenticodeSignature 'C:\Program Files\Huorong\Sysdiag\bin\HipsMain.exe'
Get-CimInstance -Namespace root/SecurityCenter2 -ClassName AntiVirusProduct |
  Select-Object displayName,productState,pathToSignedProductExe
Get-MpComputerStatus | Select-Object AMServiceEnabled,RealTimeProtectionEnabled,AntivirusEnabled
Test-Path 'C:\Program Files\Huorong\HRSafe\HrSafe.exe'     # A.4 的反证：False

# B 长路径：本机实测 324 字符可写（15 行程序 + rustc 直接编，不进仓）
#   std::fs::write(路径, b"x")  ⇒ OK；再 dir /s /b "\\?\<路径>" 确认文件在

# C Linux（Kali）
cat /sys/block/*/queue/rotational
stat -f -c '%T %S' /
```

- 火绒论坛引用：A.1/A.2/A.3 的四个帖子链接见正文；本机 `-s` **本轮未实跑**（会拉起 UI 并开始扫描），
  所以"会弹 UI、无结果输出"这两点来自官方回复，不是本轮实测。
- 代码行号对应 2026-11-22 的工作树（`git status` 里 `src/motw.rs`/`src/settings.rs` 尚未提交）；`src/` 正在改，行号会漂 —— 按函数名/错误串找（`mark_if_enabled`、`backoff_ms`、「服务器不支持 Range」「远端文件大小变了」）。

## 限制

1. 火绒部分只有**只读探测** + 官方论坛口径；`-s` 的实际行为（是否要求管理员、扫描多久、有无退出码）
   均**未实测**，上线前要真跑一次并记录。
2. A.4 的第三方页面没有逐行核对；结论是"不采用"，不是"它一定错"——只是没有任何官方或本机证据支持。
3. B 节的两条 FFI 结论（签名、防睡眠）只在"Rust 直接调用 Win32"这个前提下成立；若将来允许一个
   安全封装的 crate，结论要重算。
4. Linux 侧只有一台 Kali VM 的 `rotational=1`，不能代表真机；`fallocate` 完全未测。
5. `Retry-After` 的收益没有量化（没有做"带/不带提示"的对照实验），判据只写到"遵守服务端提示"，
   不写"能快多少"。
