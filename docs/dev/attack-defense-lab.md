# 攻防靶场：只打自己的靶

这份文档记录**我们自己**的攻防实验方法与边界。不是安全承诺，是"我们怎么找出自己的漏洞、怎么证明修好了"。

## 一、边界（先划清，再谈方法）

**做的**：

- 打**我们自己的客户端**和**我们自己起的服务端/代理**；
- 目标是"下载器在恶意或畸形响应下会不会写出坏文件、越界写、挂死、静默成功"；
- 写清可复现步骤，并尽量固化成 `tests/` 里的用例。

**不做的**（这几条与 `AGENTS.md` 铁律 3 同源）：

- 不针对第三方站点/服务器做扫描、爆破、压测、漏洞利用；
- 不写账号验证绕过、破解、去 DRM 的内容 —— 这不在项目定位里，也不在文档里；
- 不写"如何绕过某个防护"，只写"我们的防护在哪、判据是什么"；
- 不把 Kali 里的一次性手工操作当成结论：凡是能复现的都要进测试。

**为什么这么划**：这个仓库是公开发布的。一份"攻击手册"和一份"防御验证记录"的差别不在技术深度，
而在**打的是谁**。前者可被拿去打别人，后者只能用来证明自己没坏。

## 二、靶场的三块

| 块 | 用什么 | 造什么 |
|---|---|---|
| **受控源站** | `nginx`、Python `http.server`、`tests/http_semantics.rs` 里的 `Fault` 假服务器 | 任意状态码与响应头：谎报 `Content-Length`、缺 `Content-Range`、416、Range 回 200、未经请求的 gzip |
| **链路病理** | `toxiproxy`（CI 里已接）、`tc netem`（Kali 或任意 Linux）、`squid`/`tinyproxy` | 延迟、带宽、断连、停滞；代理 407 / CONNECT 被拒 / 半关闭 |
| **客户端侧输入** | 直接构造文件 | 恶意 URL、恶意 `Content-Disposition`、整合包清单里的越界路径、zip 条目名 |

Kali 在这里**没有任何特有作用**：`tc` 来自 `iproute2`，`squid`/`tinyproxy` 在哪个发行版都一样装。
它的价值只是"一台已经装好、root 可用、MCP 通道已接通"的 Linux —— 换成 Debian 也一样。
详见 [testing-infrastructure.md](testing-infrastructure.md) 的 §2.1。

## 三、实验清单（按攻击面）

每条都写：**打什么 → 我们靠什么挡 → 判据在哪**。

| # | 攻击面 | 手段 | 防线 | 判据 |
|---|---|---|---|---|
| 1 | 落点路径 | URL 末段 / `Content-Disposition` / 整合包清单 / zip 条目名里放 `../`、绝对路径、盘符、UNC | `pathsafe::{check_name, check_rel, under_root}` | `src/pathsafe.rs` 单测；`fetch --name "..\victim.txt"` 实测不越界（issue #9） |
| 2 | 内容完整性 | 远端对象中途被换（206→302→不同长度） | `Content-Range` 三项比对 + 416 报"远端当前 N 字节" + `--if-range` 降级重下 | `tests/http_semantics.rs`；`docs/reports/attack-report-20261122.md` §5 |
| 3 | 静默成功 | 服务器不声不响地压缩（`Content-Encoding: gzip`）、或无校验和 | 拒绝未经请求的压缩；`--require-sha1` | 实测：拒绝后**未落盘** |
| 4 | 挂死 | slowloris（慢 header / 慢 body / 每秒滴 1 字节） | `read_timeout` + `StallGuard`（窗口内推进不足 4 KiB 即中断） | 慢滴服务器 **10.0 s 内**失败；toxiproxy `timeout` 毒同样失败且不留成品 |
| 5 | 代理 | 代理返回 407、CONNECT 被拒、半关闭；`socks5://` 前缀 | 启动期拒绝 SOCKS（会静默直连）；错误信息区分"代理问题"与"源问题" | 单测 + 实测（`docs/user/usage.md` 的「代理：支持哪些协议」） |
| 6 | 畸形长度 | `Content-Length: -1` / 超 `u64` / 与实际不符 | hyper 拒绝 + 我们自己的长度校验（`buf.len() != expect`） | `docs/reports/attack-report-20261122.md` §3.3 |
| 7 | 分片语义 | 服务器对 Range 回 200（整份） | `RangeUnsupported` 标记 → 降级 `one_stream`，不 bail 整批 | `tests/http_semantics.rs` |

## 四、能进 CI 的与只能手工的

| 手段 | 能进 CI | 为什么 |
|---|---|---|
| `Fault` 假服务器 | 能（已经在） | 纯进程内，无外部依赖 |
| `toxiproxy` | **能（已接，`check` job）** | 单二进制，runner 本机起 |
| `nginx` / `squid` | 能，但要起容器 | 比 Python 桩更接近真实，值得后续补 |
| `tc netem` | **不能** | 要 root 改 qdisc，GitHub runner 上不稳；只能手工 |
| 真机装配（装完能否启动） | 不能 | 要客户端 + 显示；自动化收益极低 |

**规矩**：凡是只在 Kali 手工验过的，**必须在单测里补一条最小复现**（`AGENTS.md` 的硬性要求），
否则环境一变，知识就没了。

## 五、一次实验怎么写才可信

1. **口径先写、后测**：先写"我要证明什么、什么算失败"，再看结果。事后改判据等于没测。
2. **每个数可回溯**：`raw CSV → 脚本 → 图/数`，脚本放 `scripts/`，原始数据放 `docs/reports/`。
3. **没差异、被证伪、没跑，都要写**：和"有收益"一样是交付。
   本项目已经踩过"跨时刻比较造出不存在的 22 倍回归"（`.shared/postmortem/0044`）与
   "用 2 KB/s 当停滞、跑了 285 秒才发现判据写错"（`tests/toxiproxy_faults.rs` 的注释）两回。
4. **结论落成用例**：修完当次就把最小复现写进 `tests/`，别留"下次记得"。

## 六、明确不在这份文档里的东西

- 针对第三方服务器的扫描/利用步骤；
- 任何账号、授权、DRM 绕过的内容；
- 压测/DDoS 方法论；
- "Kali 全工具链教程"。这里只需要 `tc`、一个代理、一个假源。

需要这三类能力时，去对应的专业资料；这个仓库的靶场只服务于"证明我们自己没坏"。
