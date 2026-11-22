# 用法教程

本文覆盖命令行与脚本配置。五分钟能跑起来的例子在 [README](../README.md#五分钟上手)，这里讲**每个旋钮是干什么的**、以及常见场景怎么配。

---

## 一、三个入口，先分清用哪个

| 想干什么 | 用哪个 |
|---|---|
| 下一个文件 | `fetch` |
| 下一批文件 / 混合多个任务（含装 MC） | `run <脚本.toml>` |
| 只装 MC（原版 / 整合包） | `vanilla` / `pack` |

`fetch` 是"下一个文件"的默认入口，比裸 `curl` 多做四件事：大文件自动分片并行、断点续传、失败指数退避重试、可限速。

`run` 是脚本入口，"下一批"和"装 MC"都在这里编排。

### 命令怎么拼

```
mc-downloader.exe  <子命令>  <位置参数>  [--选项 值]...
                └ 必须第一个 ┘  └ 紧随其后 ┘  └ 顺序随意 ┘
```

**位置参数的顺序不能换**，每个子命令固定几个：

| 子命令 | 位置参数 |
|---|---|
| `fetch` | `<url>` |
| `probe` | `<url1> <url2> ...`（至少两个） |
| `vanilla` | `<version>` |
| `pack` | `<file>` |
| `run` | `<script.toml>` |

**`--选项` 顺序随意**——七个全局参数都标了 `global = true`，写在子命令前后都行，`--piece-conns 16` 和 `--piece-conns=16` 等价。

复制这个模板改就行：

```powershell
mc-downloader.exe fetch "<URL>" -d "<目录>" --piece-conns 16
```

> **`-n` 是可选的**：不给就从 URL 末段推导（去掉 `?query` / `#fragment`、percent-decode）。
> 推不出（URL 以 `/` 结尾、只有主机名）才回落 `out.bin`，**并且会在输出里说明**。
> 目标已存在时**默认拒绝**（退出码 1），要覆盖加 `--force`。

---

## 源：必须先自己配（issue #15）

**本工具不内置任何下载源**，官方或第三方都不内置。anilla / un / pack 都要先有源，
否则直接报错并告诉你缺哪一项：

```
Error: 未配置下载源（[source].metadata）。
  本工具不内置任何源地址 —— 这是刻意的（issue #15）。
  请先在 <配置文件> 的 [source] 段填写 metadata 地址。
  只想下单个文件？mc-downloader fetch <url> -d <dir> 不需要任何源。
```

配置文件里这么写（--config <path>，或放到平台配置目录）：

```toml
[source]
metadata    = "https://<你的源>/version_manifest_v2.json"
assets      = "https://<你的源>"
fabric      = "https://<你的源>"
forge_meta  = "https://<你的源>"
forge_maven = "https://<你的源>"
neoforge    = "https://<你的源>"
```

六项**全必填**（用到哪个加载器就得配哪个）。etch <url> 不受影响 —— 你给地址、它只做传输。

**为什么**：2026-10-20 PCL2 移除 MC 下载功能（「为遵守相关法律法规」）——「第三方工具内置下载通道」
处在合规灰区。所以「知道去哪下」由你决定，工具只负责下得对、下得快。
Forge installer 的库地址白名单**也是从你配的源推出来的**，不是写死一份 —— 换镜像不会被自己拒掉。
## 二、`fetch`：下一个文件

```bash
# 最简（落地名自动从 URL 取）
mc-downloader fetch "https://example.com/big.zip" -d "D:/Downloads"

# 指定文件名与大小（URL 末段不可靠时，比如带 id= 的查询串）
mc-downloader fetch "https://example.com/dl?id=123" -d "D:/Downloads" -n big.zip -s 314572800

# 覆盖已存在的同名文件
mc-downloader fetch "https://example.com/big.zip" -d "D:/Downloads" --force
```

| 参数 | 作用 | 不给会怎样 |
|---|---|---|
| `-d, --dir` | 目标目录 | 默认当前目录 |
| `-n, --name` | 落地文件名 | **默认从 URL 末段推导**；推不出才 `out.bin`（并在输出里说明）|
| `--force` | 目标已存在时覆盖 | **默认拒绝**：报错退出（退出码 1），不覆盖也不跳过 |
| `-s, --size` | 已知大小 | **只影响"分片还是单流"，不影响能否下载** |
| `--mirror` | 额外镜像 URL，可重复 | 单源 |
| `--no-content-disposition` | 不读服务器给的文件名 | 想严格按 URL 决定落点 |

**`--force` 为什么是必要的**（而不是默认覆盖或默认跳过）：`fetch` 的 `sha1` 为空，
而下载引擎对"同名已存在 + 无 sha1"的判定是**当作已完成直接跳过** ——
默认覆盖会丢上一个文件，默认跳过会让你以为下好了。两种都不该是默认行为，所以默认报错。

**`-n` 与 URL 推导的关系**：显式 `-n` 总是优先。推导规则与 `urls_file` 共用一份
（`url_list::filename_from_url`），所以两种入口的文件名行为一致。

**`-s` 为什么可以不给**：不给时先发 `HEAD` 探测大小；探测不到（服务器用 `Transfer-Encoding: chunked`）就**走单流下到 EOF**——照常下完，只是没法分片，大文件会慢一些。

**什么时候值得给 `-s`**：URL 带重定向或查询串、`HEAD` 返回 405、或者你已经知道确切字节数。给了就跳过探测这一步。

**落地名优先级**：`--name` > `Content-Disposition`（服务器给的，过 `pathsafe`）> URL 末段 > `out.bin`。
服务器给的名字和 URL 一样**不可信** —— `filename="../../evil"` 会被拒绝并回落，测试钉住了这条。

**`--mirror` 的正确用法**：内容必须与主 `url` **完全相同**。填错了不会报错，只会拿到坏文件（SHA1 校验能发现，但那是很久以后）。先用 `probe` 确认，见第四节。

---

## 三、`run`：批量与编排

脚本是 TOML，`[[task]]` 数组按声明顺序执行：

```bash
mc-downloader run my.toml
mc-downloader run my.toml --parallel   # 多个 task 并行（总并发会被均分）
```

### 3.1 批量下 URL 列表

一个 task 就能下完一整张列表，**不是每行起一个进程**：

```toml
[[task]]
kind = "generic"
urls_file = "list.txt"
dir = "D:/Downloads"
```

`list.txt` 一行一个 URL，空行与 `#` 注释跳过。**格式对齐 `aria2 -i`**——现成的 aria2 列表文件可以直接拿来用（`dir=` / `out=` 这类选项行会被跳过并计数）。

**`urls_file` 模式下不能给的字段**：`name` / `size` / `mirrors`。给了直接报错，因为多文件共用一个名字没有意义，允许就等于静默不生效。

**两个 URL 推出同一个文件名也会报错**——否则后者会被当成"已存在"跳过，静默少下一个。

### 3.2 批量下载的失败语义

这里有个容易踩的差异：

| 场景 | 一个文件失败时 |
|---|---|
| MC 装配（`vanilla` / `pack`） | **整批中止**（少一个文件游戏就坏） |
| `urls_file` 批量 | **继续下其余的**，最后仍然返回非 0 并列出失败项 |

批量场景下最怕的是"静默少下一个"——所以退出码非 0 是必须看的信号，别只看"下完了"。

### 3.3 一个脚本里混合多种任务

```toml
# 先装原版 + Fabric
[[task]]
kind = "mc"
version = "1.20.1"
loader = "fabric"
dir = "D:/Games/.minecraft"

# 再下几个资源包
[[task]]
kind = "generic"
urls_file = "resourcepacks.txt"
dir = "D:/Games/.minecraft/resourcepacks"

# 最后装一个整合包
[[task]]
kind = "pack"
file = "D:/Downloads/xxx.mrpack"
dir = "D:/Games/.minecraft"
```

按声明顺序执行；加 `--parallel` 则并行（`concurrency` 会被均分给各 task，不会 N×16）。

---

## 四、`probe`：先测再选源

```bash
mc-downloader probe <url1> <url2> [...] --seconds 5
```

串行各测 N 秒，**输出按吞吐降序排名**。把快的填进 `mirrors`。

**为什么不并发测**：并发会互相抢带宽，测出来的排名是假的。串行慢一点，但每个数都是真的。

**`probe` 测的是单连接吞吐**——它回答"这些源里哪个快"，不回答"加连接还能不能再快"。后者的判据见第五节 `--piece-conns`。

实测差距可以很夸张（同一台机器、同一代理）：

```
GitHub release :   25–92 KB/s
清华 TUNA      : 10.9–35 MB/s      ← 差 400 倍
```

`mirrors` 填好后，下载时按实测吞吐（EWMA）**把分片分配给各源**——不是"主源挂了才用镜像"，而是**一开始就按快慢分配**。

> 本工具**不内置任何非官方镜像**，也不自动把 GitHub 换成第三方镜像。`probe` 只负责测，用哪个源由你决定。

---

## 五、调优：六个全局参数

所有参数都是 `global = true`，**写在子命令前后都行**：

```bash
mc-downloader --concurrency 8 run my.toml
mc-downloader run my.toml --concurrency 8      # 等价
```

| 参数 | 默认 | 什么时候改 |
|---|---|---|
| `--concurrency <N>` | 16 | **同时下几个文件**。文件多而小（< 1 MB）时可调高；网络差时调低 |
| `--piece-conns <N>` | 4 | **单文件分片几条连接**。见下 |
| `--adaptive <bool>` | 关 | AIMD：快则 +1、慢则减半，worker 上限 `2 × piece-conns` |
| `--retry <N>` | 3 | 单文件失败重试次数，指数退避 1s / 2s / 4s… |
| `--timeout <秒>` | 30 | **建连超时与单次读超时**，不是整请求总时限 |
| `--max-speed <5M>` | 不限 | **全局总带宽**，不是每连接一份 |
| `--if-range` | 关 | 分片带 `If-Range`：远端中途被换掉时**立即降级成单流重下**，而不是把两代内容拼起来。见下 |

**两个别名**：`--conns` = `--piece-conns`，`--jobs` = `--concurrency`（issue #14：想在 `fetch` 上临时调并发，
不必去改配置文件）。它们都是 `global = true`，写在子命令前后都行。

### `--if-range`：什么时候开

默认**关**。打开后，同一个源的后续分片会带上首个响应的**强验证器**（ETag；弱 ETag 按 RFC 不用，退 `Last-Modified`）。
远端若在中途被换掉，服务器会回 200（整份）⇒ 我们按既有逻辑降级成单流重下。

**为什么默认关**：多镜像/多 CDN 各自算 ETag，拿 A 的 ETag 去请求 B 必然不匹配 ⇒ 服务器回 200 ⇒
"能用但慢"的镜像会被直接判死。所以验证器**按源**缓存，且开关交给用户。

**判据**（实测，32 MiB / `--piece-conns 1`）：

```
带 --if-range   ⇒ 分片 2 片 → 第 2 片条件不成立 → 「已退回单流下载」→ sha1 一致
不带 -if-range  ⇒ 分片 2 片 → 正常完成 → sha1 一致
```

### 代理：支持哪些协议

- `--proxy http://…` / `https://…` **可以用**。
- `socks4` / `socks4a` / `socks5` / `socks5h` **不能用**：本 build 的 reqwest 没开 `socks` feature，
  而那个代理会被**静默忽略**（流量直连 —— 2026-11-23 实测过：配 `socks5://127.0.0.1:1080` 后，
  报的是"连接目标被拒"，说明根本没走代理）。所以现在**启动期直接报错**，不假装走代理。
  要走 SOCKS 请自行用 `--features reqwest/socks` 重新编译。
- **不配 `--proxy` 就是直连**：我们调 `no_proxy()`，**不读** `HTTP_PROXY` / `HTTPS_PROXY` 等环境变量
  （可复现性靠这条）。

### `--piece-conns` 到底该给多少

4 这个默认值不是随手定的——文献实测最优并行 TCP 连接数在 4~12 之间（Netscape 历史上就选 4）。

但**多连接的本质是"抢"，不是"增"**：

- 服务器**不限速**、带宽够时 ⇒ 纯抢占：对别人不公平，对自己也没真变快
- 单连接**被限速**时（如 GitHub 的 25–92 KB/s）⇒ 才是解药，实测中位 **4.93 倍**加速

所以：**只在「单连接明显被限速」的源上加 `piece-conns`**。正常源加了反而可能更慢（小文件尤其——2.4 MB 样本上 4 并发比单连接慢 3 倍，握手开销摊不平）。

分片有门槛：**≥ 8 MB 才切分片**，单片刻度 1–16 MB。

#### 怎么判断"这个源该加几条"

**实测（2026-10-30，阿里云镜像，924 MB 文件，直连，各测 5 秒）**：

| 连接数 | 吞吐 | 924 MB 耗时 |
|---|---|---|
| **4（默认）** | 3.0 MB/s | 约 5 分钟 |
| 8 | 7.0 MB/s | 约 2.2 分钟 |
| **16** | **15.2 MB/s** | **约 1 分钟** |

**4 条只有 16 条的 1/5** —— 阿里云这类国内镜像对单连接限速在 0.75–1 MB/s，总吞吐随连接数近似线性增长。

> 注意：**`probe` 测不出这个**。它测的是**单连接**吞吐，
> 所以 `probe` 给你 1 MB/s 并不代表"这源就这样了"——它只告诉你"一条连接的价"。
>
> 判据：**`probe` 的单连接值明显低于你的实际带宽时，就该加 `--piece-conns`。**
> 反过来，如果 `probe` 已经跑满带宽，加连接不会更快（纯抢占）。

**经验值**：你自己配的公共镜像站可以先试 `--piece-conns 16`；
GitHub、境外 CDN 先用 8；不确定就开 `--adaptive true` 让它自己调（上限 `2 × piece-conns`）。

#### 还有一条：目标盘是机械盘时，**别加**

上面说的都是"源在限速"。反过来还有第二种情况：**源很快、但盘是机械盘**。

实测（2026-11-22，1 GiB，目标盘是 USB 外置机械盘，三轮取中位；原始数据见 `reports/quant-report-20261122.md` 的 E2）：

| `--piece-conns` | 墙钟 | 物理写放大 |
|---|---|---|
| 1 | **21.4 s** | **1.006×** |
| 4（默认） | 34.2 s | 1.345× |
| 16 | **44.0 s** | **1.762×** |

同一台机器把同样文件写到内置 NVMe 时**没有任何惩罚**（3–4 s）。⇒ 瓶颈不在网络、不在 CPU，
在「**片乱序落盘 + 机械盘**」：并发让片按完成顺序乱序写，机械盘要为每次跳变付寻道。
写放大与介质无关（NVMe 也是 ~1.6×，更像 NTFS 的 VDL 零填充），但**只有机械盘把它变成墙钟惩罚**。

⇒ 目标盘是机械盘时，**大文件用 `--piece-conns 1`**（片仍会切，只是顺序下、顺序写）比默认更快。
根治方案（按片序写 / 预分配）还没做，见 issue #8。

### `--max-speed` 的语义

`5M` / `500K` / 纯数字（字节/秒），1024 进制。是**全局总带宽**——所有文件、所有分片共享一个令牌桶。

实测 `--max-speed 100K` 跑出 85–88 KB/s（令牌桶稳态效率，低于设定值是正常的）。想边工作边下就先设这个。

---

## 六、脚本格式参考

```toml
schema = 1            # 可选。省略按 1 处理；写别的数会明确报「太新/太旧」，而不是让字段静默失效

continue_on_error = false   # 可选，**脚本级**：某个任务失败时是否继续跑后面的（末尾汇总并以非零退出）

[network]
proxy = "http://127.0.0.1:7897"   # 空字符串 "" = 显式直连
retry = 3
timeout_secs = 30

[download]
concurrency = 16
piece_conns = 4
adaptive = false
max_speed = "5M"
```

### 6.1 逐任务覆盖（`retries` / `timeout_secs` / `max_speed`）

三个键可以写在**任意** `[[task]]` 上，只影响那一个任务：

```toml
[[task]]
kind = "generic"
url = "https://<你的源>/big.iso"
dir = "D:/Downloads"
retries = 1          # 这一个大文件不值得重试三次
timeout_secs = 120   # 慢连接上单次读 30s 不够
max_speed = "20M"    # 别的任务不限速，这一个限到 20M
```

优先级：**CLI > 逐任务 > 脚本全局（`[network]`/`[download]`）> 用户配置 > 内置默认**。

取值在**启动期**校验：`retries = 0`、`timeout_secs` 越界、`max_speed` 写错，都会在开始下载之前报错
——不会等到跑到一半才失败。

### 6.2 为什么 `continue_on_error` 在脚本级而不是任务级

"失败后要不要继续"是**这一次运行**的属性，不是某个任务的属性。写在任务上会变成
"第 2 个任务失败时，第 3 个怎么办取决于第 2 个自己的配置" —— 那是逻辑陷阱，不是灵活性。

行为：为 `true` 时逐条打印「第 N 个任务失败（已跳过）」并继续，**末尾汇总失败数并以非零退出**；
绝不"跳过失败还报成功"。

```toml
[[task]]
kind = "mc"          # 装原版（可选加载器）
version = "1.20.1"
dir = "."
loader = "fabric"    # 省略 = 只装原版；"fabric" / "fabric:0.20.0"
```

```toml
[[task]]
kind = "generic"     # 下 URL
url = "..."          # 与 urls_file 二选一
urls_file = "..."    # 与 url 二选一
dir = "D:/Downloads"
name = "..."         # 仅 url 模式
size = 314572800     # 仅 url 模式
mirrors = ["..."]    # 仅 url 模式
```

```toml
[[task]]
kind = "pack"        # 装整合包（.mrpack 或结构相同的 .zip）
file = "xxx.mrpack"
dir = "."
```

**校验规则**（不满足直接报错，不会带病执行）：

- 至少一个 `[[task]]`
- `concurrency` 在 1..=1024，`piece_conns` 在 1..=256
- `generic` 的 `size = 0` 会报错（会导致不分片），请给真实大小或省略
- `url` / `urls_file` 二选一；`urls_file` 模式下禁用 `name` / `size` / `mirrors`
- 未知字段报错（`deny_unknown_fields`）——拼错字段名不会静默忽略

---

## 七、代理：默认直连

**默认直连**。Mojang CDN / Fabric meta / Modrinth CDN 在国内均直连可达。

只有直连拿不到（超时 / `000`）时才加 `--proxy`：

```bash
mc-downloader --proxy http://127.0.0.1:7897 fetch "https://..."
```

脚本里写 `proxy = ""`（空字符串）表示**显式直连**，用于覆盖环境里可能存在的代理设置。

---

## 八、排错

| 现象 | 先查 |
|---|---|
| 下到一半失败，重跑变快了 | 正常：`.part.state` 记了已完成的分片，只补缺口 |
| 文件大小对但打不开 / 校验不过 | `mirrors` 填错了（内容不同源）；或服务器返回了错误页 |
| `--max-speed` 跑不满设定值 | 正常：令牌桶稳态效率低于设定（实测 `100K` 跑出 85–88 KB/s） |
| 批量下少了一个文件却没报错 | 看退出码——失败项会让退出码非 0 |
| `No matching version`（装 MC） | 版本号写错，或该加载器不支持这个 MC 版本 |
| 装完启动不了 | 本工具只保证**目录结构**，是否真能启动请用 PCL / HMCL 实际打开一次 |

---

## 九、用于下载你有权下载的文件

本工具不判断使用者是否持有相应授权，这一点由使用者自行负责。
