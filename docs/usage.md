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

---

## 二、`fetch`：下一个文件

```bash
# 最简
mc-downloader fetch "https://example.com/big.zip" -d "D:/Downloads"

# 指定文件名与大小
mc-downloader fetch "https://example.com/dl?id=123" -d "D:/Downloads" -n big.zip -s 314572800
```

| 参数 | 作用 | 不给会怎样 |
|---|---|---|
| `-d, --dir` | 目标目录 | 默认当前目录 |
| `-n, --name` | 落地文件名 | 默认 `out.bin`（URL 末段不可靠时用它） |
| `-s, --size` | 已知大小 | **只影响"分片还是单流"，不影响能否下载** |
| `--mirror` | 额外镜像 URL，可重复 | 单源 |

**`-s` 为什么可以不给**：不给时先发 `HEAD` 探测大小；探测不到（服务器用 `Transfer-Encoding: chunked`）就**走单流下到 EOF**——照常下完，只是没法分片，大文件会慢一些。

**什么时候值得给 `-s`**：URL 带重定向或查询串、`HEAD` 返回 405、或者你已经知道确切字节数。给了就跳过探测这一步。

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

### `--piece-conns` 到底该给多少

4 这个默认值不是随手定的——文献实测最优并行 TCP 连接数在 4~12 之间（Netscape 历史上就选 4）。

但**多连接的本质是"抢"，不是"增"**：

- 服务器**不限速**、带宽够时 ⇒ 纯抢占：对别人不公平，对自己也没真变快
- 单连接**被限速**时（如 GitHub 的 25–92 KB/s）⇒ 才是解药，实测中位 **4.93 倍**加速

所以：**只在「单连接明显被限速」的源上加 `piece-conns`**。正常源加了反而可能更慢（小文件尤其——2.4 MB 样本上 4 并发比单连接慢 3 倍，握手开销摊不平）。

分片有门槛：**≥ 8 MB 才切分片**，单片刻度 1–16 MB。

### `--max-speed` 的语义

`5M` / `500K` / 纯数字（字节/秒），1024 进制。是**全局总带宽**——所有文件、所有分片共享一个令牌桶。

实测 `--max-speed 100K` 跑出 85–88 KB/s（令牌桶稳态效率，低于设定值是正常的）。想边工作边下就先设这个。

---

## 六、脚本格式参考

```toml
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
