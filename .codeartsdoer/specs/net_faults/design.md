# design — net_faults

> 2026-10-21 ｜ 上游：`spec.md`（同目录，rev2）
> 五维预读：① spec ② `AGENTS.md`（**本项目无**，已核）③ `postmortem/`（**无同族教训**）
> ④ 存量代码：`download.rs` / `piece.rs` / `client()` ⑤ 外部实现：curl `sws` 的「按剧本造故障」思路
>
> **rev2 修订**：经两道定稿门审计（23 条不一致 + 6 条待澄清），已修正场景档位、接口缺口、
> 单位混淆、判据对象错位等；详见 §10 修订记录。

---

## 1. 实现模型

### 1.1 上下文视图

```
┌─ Kali VM（root）────────────────────────────────────┐
│                                                     │
│   fakeorigin.py (127.0.0.1:8899)                    │
│          ↑                                          │
│      lo 接口  ← tc netem / iptables（故障只在这层）  │
│          ↓                                          │
│   mc-downloader fetch ──→ out/t.bin                 │
│                                                     │
│   scenarios/*.sh  做三件事：设故障 → 跑 → 判 PASS    │
└─────────────────────────────────────────────────────┘

┌─ GitHub CI（ubuntu, 无 root）──┐
│ tests/linux_semantics.rs       │  只做平台语义
└────────────────────────────────┘
```

### 1.2 组件总体架构

| 组件 | 职责 | 不做什么 |
|---|---|---|
| `fakeorigin.py` | 按 Range 返回字节 | **不模拟故障**（故障归 netem）|
| `lib.sh` | 起源、设/清 netem、跑被测、比对 sha1、打印判定 | 不含具体场景 |
| `scenarios/*.sh` | 一条场景 = 一个文件，调 `lib.sh` | 不重复实现 lib |
| `run_all.sh` | 遍历场景、`--repeat` 控制重复次数、汇总出报告 | 不含场景逻辑 |
| `tests/linux_semantics.rs` | Linux 平台语义 | 不碰网络 |

**分层理由（来源：`DESIGN §11.3` 的两处接缝原则）**：
故障施加点与「造数据」点必须分离。一旦 `fakeorigin.py` 也开始模拟慢/丢包，
就分不清失败是「源的问题」还是「网络的问题」——**归因会失效**。

### 1.3 关键决策（含被否方案）

| # | 决策 | 理由 | 被否方案 |
|---|---|---|---|
| **D1** | 被测端跑在 **VM 内** | 单机闭环、路径最短、无需跨机传二进制、故障隔离清楚 | 被测端在本机、netem 在 VM：需要 VM 能访问本机假源，多一跳且归因变难 |
| **D2** | netem 加在 **`lo`**，但**先把 `lo` 的 MTU 改成 1500** | `lo` 的 MTU 是 65536，netem 按**包**调度；不改 MTU 时丢包惩罚被放大约 43 倍（一个 64KB 段丢一次 = 丢 64KB，而真实链路丢的是 1500 字节），测出来的不是链路行为 | ① **给 netem 传 `mtu` 参数：实测不存在**（见下方 rev3）② veth pair + 双命名空间：更真实但复杂，留作退路 |
| **D3** | 假源**不做任何限速**，限速**只由 netem 施加** | 双重限速会让「实测吞吐 vs 限速值」的比对失去意义（不知道是谁在卡）| 源端 `sleep` 限速：正是本决策要排除的 |
| **D4** | 测试文件由**脚本在 VM 内生成** | 不传大文件；`bytes(i%251)` 保证非均匀 | 从本机 scp 128MB：慢且引入传输失败变量 |
| **D5** | Linux 语义走 **CI**，L2/L2′ **不进 CI** | 语义测试不需要 root；需要 root 的进 CI 既跑不了也不该跑（CI 会 `iptables` 影响构建机）| 全塞进 CI：需要 `sudo`，污染 runner 网络 |
| **D6** | AIMD 观测**加一个环境变量控制的诊断输出** | 只看吞吐无法归因「是 tuner 在动，还是网络在动」，必须看到 permit 数 | ① 只解析进度行推断：无法归因 ② 加完整 metrics 模块：为一个验证拉抽象，过重 |
| **D7** | 脚本用 **bash**，不用 Rust 测试框架 | 需要 root + `tc` + 子进程编排，Rust 里做这些是绕路 | `#[ignore]` 集成测试 + `std::process::Command`：仍需 sudo，且难清场 |
| **D8** | 期望 sha1 用**离线预算的常量**，写进场景文件 | 运行时现算会让「生成端写错」与「下载端复制同样错误」自洽通过（两侧同错，测不出任何东西）| 由 `fakeorigin.py` 启动时算：正是被否的那种 |

#### D6 的边界（**必须写死，否则会成为"为了测试改生产代码"的先例**）

只允许这种形状，且**默认关闭**：

```rust
// download.rs 的 tuner 循环内，紧随 available_permits() 之后
if std::env::var_os("MC_TUNE_LOG").is_some() {
    eprintln!("[tune] avail={avail} delta={delta}");
}
```

三条约束：
1. **只输出，不改任何状态** —— 不参与 `if delta > last_delta` 的判断
2. **默认关闭**（无环境变量时零行为差异）
3. **不允许**在别处为测试加分支；若将来需要更多观测点，改为独立的 `tracing` 层，不再散落 `eprintln!`

> spec §3.1 的「不修改其内部」由 spec §5.4 的「唯一授权例外」覆盖，范围仅限上述 8 行。

#### D2 的实测修正（**rev3，2026-10-21 在 Kali VM 上实捕**）

**`netem mtu 1500` 不可用。** iproute2-7.2.0 的 netem 参数清单里**没有 `mtu`**：

```
limit / delay / distribution / corrupt / duplicate / loss / seed / ecn / reorder / rate / slot
```

我记错了（以为 netem 支持按 MTU 切包）。替代方案**已实测可行**：

```bash
sudo ip link set dev lo mtu 1500              # ✅ 成功，ip link show lo 确认 mtu 1500
sudo tc qdisc add dev lo root netem loss 5%   # ✅ 正常挂载
sudo tc qdisc del dev lo root                 # ✅ 可删
sudo ip link set dev lo mtu 65536             # ✅ 可恢复
```

⇒ **`lib.sh` 的 `apply` / `clear` 要成对管理 MTU**：`apply` 前设 1500，`clear` 时恢复 65536。

⚠️ **遗留待验（T3.1 负责）**：改 MTU 后 netem 是否**真按 1500 粒度算包** ——
`lo` 上仍可能有 GSO 合并，内核在 netem 之后才切分。
判据：**同一 loss 率下，MTU 1500 与 MTU 65536 的传输耗时应有显著差异**。
若两者无差异 ⇒ MTU 改动未生效 ⇒ **此时才转 veth pair**（并回写本文件、重走定稿门）。

#### `iptables` 删除的坑（同批实捕）

**`iptables -D` 必须带完整 spec**，包括 `--reject-with tcp-reset`。
实测：建规则时带 `--reject-with`、删除时不带 → `Bad rule (does a matching rule exist in that chain?)`，
**规则残留**，此后该端口所有连接被 RST，后续场景全线污染。

```bash
# ❌ 删不掉
sudo iptables -D OUTPUT -p tcp --sport 8899 -j REJECT
# ✅ 完整 spec
sudo iptables -D OUTPUT -p tcp --sport 8899 -j REJECT --reject-with tcp-reset
# ✅ 或按行号（但行号会随规则增删变化，需先查）
sudo iptables -D OUTPUT 1
```

⇒ `clear` 删完**必须自检** `iptables -L OUTPUT -n` 只剩表头，否则报错退出。

---

## 2. 接口设计

### 2.1 总体设计

**脚本即接口**。目录约定就是契约，新增场景不改框架。
**接口清单的唯一真相源是 §2.2**，本目录树不重复列举接口名（避免两份清单互相漂移）。

```
scripts/net_faults/
├── fakeorigin.py            # Range 源；**不限速**（D3）
├── lib.sh                   # 核心框架，接口见 §2.2
├── run_all.sh               # 遍历 scenarios + --repeat + 生成 report.md
└── scenarios/               # 共 14 个，见下
    ├── l2_rate_800kbit.sh   ├── l2_delay_20ms.sh    ├── l2p_loss_5.sh
    ├── l2_rate_8mbit.sh     ├── l2_delay_200ms.sh   ├── l2p_loss_20.sh
    ├── l2_rate_80mbit.sh    ├── l2_rst_mid.sh       ├── l2p_reorder_10.sh
    ├── l2_rate_400mbit.sh                           ├── l2p_reorder_25.sh
    ├── l2p_loss_1.sh                                ├── l2p_dup_10.sh
    └── aimd_step.sh
```

> **命名规则：带宽档位一律带 `bit` 后缀**（`800kbit` / `8mbit` / …）。
> 曾经的 `l2_rate_1m` 同时被解读成 `1mbit` 与 `1MB/s`（差 8 倍），是最容易埋雷的地方。
>
> 场景总数 **14**（spec §5.1.1 + §5.2.1 + §5.4 逐条相加），不得自行计数。

### 2.2 接口清单

| 接口 | 签名 | 契约 |
|---|---|---|
| `load_env` | `load_env` | 设 `IFACE`(默认 lo)、`PORT`(8899)、`SIZE_MB`(16，场景可覆盖)、`BIN`、`OUT` |
| `start_origin` | `start_origin <size_mb> <期望sha1>` | 生成文件并起源；**比对生成结果的 sha1 与入参常量**（不符即 FAIL，D8）|
| `apply` | `apply <netem 参数>` | 幂等：先 `qdisc del` 再 `add` |
| `clear` | `clear` | **必须可重复调用**；trap EXIT 也要调。**必须做四件事**：删 netem · 删 iptables 规则（带完整 spec 或按行号）· **恢复 `lo` MTU 到 65536** · 杀假源。删完**自检**（`tc qdisc show` 无 netem、**`iptables -L OUTPUT -n` 里不再有 `--sport $PORT` 的规则**），否则报错退出。<br>⚠️ **不能判「只剩表头」** —— 那假设 OUTPUT 链里没有别人的规则；docker / ufw 随便加一条就会让**每个场景的 clear 都 FAIL**，且报错内容是"有 3 行"而不是"哪条不是我们的" |
| `run_case` | `run_case <name> <断言1> [参数...] [-- <断言2> ...]` | 跑被测 → 采指标 → 逐个调断言 → 打印**合并的一行**。**实测值经全局变量注入**：`CASE_RC` / `CASE_ELAPSED_MS` / `CASE_BYTES` / `CASE_RATE_BPS` / `CASE_PEERS`（**运行期采样文件路径**）/ `CASE_OUT`。三个内置断言自动取值（`assert_rate`←`CASE_RATE_BPS`、`assert_sha`←`CASE_OUT`、`assert_local_only`←**`CASE_PID`**）；自定义断言直接读全局。**`--` 用于一行装多个断言**（如 `assert_rate 8000000 -- assert_sha "$SHA16"`）。<br>⚠️ **`run_case` 必须有「本轮」的概念**：起跑前清空 `CASE_OUT`，采集时校验 `mtime >= t0`，否则判 `out=stale`。否则上一轮残留的成品会让 sha 断言**假 PASS**。判定行恒定带 `rc=$CASE_RC` |
| `assert_rate` | `assert_rate <实测bps> <目标bps> <容差>` | 默认容差 `0.2`（即 0.8~1.2×）|
| `assert_sha` | `assert_sha <文件> <期望>` | 逐字节一致 |
| `assert_local_only` | `assert_local_only [pid]` | 核对对端**全是 127.0.0.1**。注入来源是 **`CASE_PID`**（不是 `CASE_PEERS` —— 后者是运行期采样文件路径）。⚠️ **`peers==0` 必须判失败**（`no_evidence`）：没观察到不等于干净。显式传 pid 时跳过注入 |
| `assert_rc` | `assert_rc <期望rc\|nonzero>` | 读 `CASE_RC`；`nonzero` 表示「非 0 即通过」（T3.3 的 RST 场景用）。detail 只补 `expect=`，**rc 由 `run_case` 恒定给出，不重复** |
| `emit_report` | `emit_report <输出路径>` | 把本次所有判定行写成 `report.md` 的表 |
| `run_all --repeat N` | `run_all.sh --repeat N` | 重复 N 次并逐次比对结论（spec §4.2 要求 N=3）|
| `fakeorigin.py` | `python3 fakeorigin.py --size-mb N --port P [--verify SHA]` | 起假源；启动打印 `SIZE=<bytes>`。内容规则 `bytes(i%251)`；**不内置限速**（D3）；`--verify` 给出时自校验生成结果，不符即 `exit 1` |

**`run_case` 维护的附加全局**（除表格里那六个之外）：

| 全局 | 含义 |
|---|---|
| `CASE_STALE` | 产物 `mtime < t0` ⇒ `1`，判定行出 `out=stale(...)` 并**强制 FAIL** |
| `CASE_OUT_MTIME` | 产物 mtime（epoch 秒），用于 stale 判定与诊断 |
| `CASE_PEERS_ERR` | `ss` 的 stderr（**不许吞**），以 `ss_err=` 进 detail；**写入前截断到 ≤3 条**（否则 120s 可累积 17KB 拼进一行判定）|

⚠️ **`CASE_OUT` 的语义约束**（两条都防"测试自己毁掉自己的证据"）：

1. **`CASE_OUT` 必须指向最终成品**。跨 `run_case` 需要保留的状态（`.part` / `.part.state`）**不得**放在这个路径上
   —— 否则第二次 `run_case` 起跑的 `rm` 会把续传状态删掉，T3.3 自毁。
2. **谁清 `.part*`**：`lib` 只清 `CASE_OUT`，**不碰 `.part` / `.part.state`**。
   场景若需干净起步必须自己在准备段删，**不得**指望 lib ——
   残留的 `.part.state` 会让 T4.1 的 `part_state=absent(expect=...)` 拿到**上一轮的证据**，
   这是 P0-1 的同一缺陷类换了个文件。

> ⚠️ **`fakeorigin.py` 必须开 `allow_reuse_address = True`**（SO_REUSEADDR）。
> 不开的话，上一条场景留下的 **TIME_WAIT** 会让 `bind()` 直接失败，
> 表现为「假源启动即退出 / 端口被占」——而 `ss -tan` 里只有 `TIME-WAIT` 没有 `LISTEN`，
> 极易误判成"有残留进程"。
> **14 个场景连跑 + `--repeat 3` 时这必然发作**，且是随机 flake。

> ⚠️⚠️ **`request_queue_size` 必须 ≥ 场景最大并发数**（stdlib 默认只有 **5**）。
> 不设的话，>5 条同时连接会让 listen 队列溢出 → **SYN 被丢** → 客户端按 RTO 退避重传（1s、2s）
> → **整个传输被 SYN 重传主导**。
>
> 实测（Kali VM，128MB × 32 连，各跑 3 次；判据是内核 `ListenOverflows` 增量）：
>
> | 变体 | 峰值 ESTAB | `ListenOverflows` | 耗时 |
> |---|---|---|---|
> | `request_queue_size=5`（默认）| 8–9 | **+42 / +35 / +29** | **2141 / 2100 / 2104 ms** |
> | `request_queue_size=128` | **27–36** | **0 / 0 / 0** | **101–105 ms** |
>
> **2.14 s → 0.10 s，约 21 倍。**
> 负向对照（16MB × 4 连，4 ≤ 5）：`+0` —— 证明计数器只在**真溢出**时增加。
>
> ⚠️ **`overflows = 0` 才是决定性判据**（累计量，不依赖采样率）；
> 「峰值」是采样量，会出现 36 > 名义 32 的情况（刚完成但 socket 尚未关闭的连接被计入）。
> ⇒ **期望值写「27–36」，不要写死 32。**
>
> **后果不止"并发爬不满"**：任何 >5 连的测量都会被污染 ——
> 吞吐被 SYN 重传主导、耗时翻倍、**AIMD 阶跃窗口被吃掉**（T5.1 每段才 25s，2–3s 的重传就占 1/10）。
> 而且**归因会指向被测程序**：假源把「源端排队」伪装成「客户端慢」，正是 D3 要排除的那种失真。
>
> **取值 128**：`net.core.somaxconn = 4096`，不会被内核截断；我们最大并发 64（`--piece-conns` 与 `--parallel`），
> 留一倍余量。再往上只是内核队列内存，没有行为收益。

**跨任务的自定义 CLI 必须在这里写死**（`start_origin` 与 `fakeorigin.py` 分属两个任务，
若不书面约定，两边必然各造一套参数名）。当前约定：
`--size-mb <N>` · `--port <P>` · `--verify <sha1>`（可选）。

**判定行格式（机器可解析，带宽一律 bps）**：

```
PASS l2_rate_8mbit rc=0 rate=982000 expect=1000000 tol=0.2 sha=ok
FAIL l2p_loss_20 rc=143 timeout=120s part_state=present(expect=absent)
```

> ⚠️ **示例必须与契约同源**：`rc=` 恒定出现在**每个**判定行（PASS 也有 `rc=0`）。
> 上面这两行是从一次真实运行的格式抄的，**改契约时同步改示例**，别让机器解析方照旧正则会匹配不上。

第二条同时是失败证据：**说清哪一项不符**，不是只打 `FAIL`。

---

## 4. 数据模型

### 4.1 设计目标

| 目标 | 约束 |
|---|---|
| 场景可枚举 | 一个场景一个文件，目录即清单 |
| 结论可复核 | 每行含实测值与期望值，人能看懂 |
| 失败可定位 | 失败行必须指出**不符的那一项** |
| 无状态残留 | 任何场景结束后 `tc qdisc show` 应为空；`.part.state` 按预期存在或不存在 |

### 4.2 模型实现

**场景元数据不建库、不建表**，就放脚本头部注释，由 `run_all.sh` 用 `sed` 提取。
字段与 spec §6.1 一一对应：

```bash
# NAME: l2_rate_8mbit          # 由文件名派生亦可，显式写便于 grep
# LAYER: l2                    # l2 | l2p
# NEEDS_ROOT: yes
# TIMEOUT_S: 120
# ASSERT: assert_rate          # 必填，run_all.sh 用它拦截「有动作无断言」
# SIZE_MB: 16                  # 可选，覆盖 load_env 默认
```

**`ASSERT` 是 spec §5.5「禁止有动作无断言」的唯一静态载体** ——
`run_all.sh` 遍历时若某场景缺 `ASSERT`，直接判 `FAIL(missing_assert)`，不执行。

**为什么不建 JSON 清单**（被否）：场景与脚本强绑定，两份数据必然漂移（`ssd-workflow §九 坑 3`：
「两处清单数不符 = 漂移信号」）。注释即唯一真相。

---

## 5. 改动点清单

| 文件 | 动作 | 行数估计 | 说明 |
|---|---|---|---|
| `scripts/net_faults/fakeorigin.py` | 新建 | ~80 | Range 源，**去掉内置限速**（D3）|
| `scripts/net_faults/lib.sh` | 新建 | ~190 | 核心框架（§2.2 表格共 12 行，其中 **10 个是 shell 函数**；另 2 行 `run_all --repeat` 与 `fakeorigin.py` 不在此文件）|
| `scripts/net_faults/scenarios/*.sh` | 新建 **14** 个 | 各 ~35 | 一条场景一文件 |
| `scripts/net_faults/run_all.sh` | 新建 | ~90 | `--repeat`、`ASSERT` 校验、`report.md` |
| `scripts/net_faults/expected_sha1.sh` | 新建 | ~20 | **离线**算出 `bytes(i%251)` 各尺寸的 sha1（D8）|
| `tests/linux_semantics.rs` | 新建 | ~200 | 5 组 `#[cfg(target_os = "linux")]` 测试 |
| `src/download.rs` | **修改** | +8 | D6 的 `MC_TUNE_LOG` 钩子 |
| `.github/workflows/ci.yml` | **确认，不改** | +0 | 已跑 `--all-targets --locked`，`tests/` 会被自动纳入；tasks T1.3 做一次核对即可 |
| `DESIGN.md` §8.2 / §11 | 修改 | +20 | 状态回填 |

**生产代码只动 `download.rs` 一处 8 行，且默认关闭。** 其余全在项目外的脚本与测试目录。

---

## 6. 测试接缝与 mock 策略

| 被测能力 | 接缝 | 真实 / mock | 理由 |
|---|---|---|---|
| 分片下载与续传 | CLI `fetch` | **真实**（假源 + netem）| 判据就是真实 IO |
| 限速下的吞吐 | 进度行 / 总耗时 | 真实 | — |
| RST 后的重试 | 进程退出码 + `.part.state` | 真实 | 要看**进程活着**时的分支 |
| AIMD 内部状态 | `MC_TUNE_LOG` 输出 | 真实 | 见 D6 |
| Linux 平台语义 | `std::fs` 直接调用 | **真实文件系统，不打桩** | 打桩就测不到平台差异（本 feature 的唯一目的）|
| 故障施加本身 | `tc` / `iptables` | 真实 | 无替代 |

**明确不打桩的地方**：`tests/linux_semantics.rs` 里**不使用**内存文件系统、
不 mock `std::fs`。必须落在真实 ext4/overlayfs 上，否则测的是假的东西。

---

## 7. Linux 平台语义的具体判据（`tests/linux_semantics.rs`）

| # | 测试 | 判据 | spec |
|---|---|---|---|
| 1 | `case_sensitive_paths` | 建 `A` 与 `a`，`exists()` 各自为真、内容不同 | 5.3 |
| 2 | `sparse_set_len` | `set_len(1GB)` 后 `blocks()*512` ≪ 1GB；fs 不支持稀疏时输出 `SKIP` 并**在 CI 上报失败**（`REQUIRE_SPARSE=1`）| 5.3 |
| 3 | `rename_overwrites` | A 存在时 `rename(B, A)` 成功且内容是 B 的 | 5.3 |
| 4 | `unlink_open_file` | 打开句柄后 `remove_file` 成功，**且删除后原句柄仍能读出完整内容**、`metadata().len()` 未失效。（**不做** `/proc/self/fd` 计数断言：`cargo test` 多线程共用进程，计数受其它测试干扰，硬断言必 flaky）| 5.3 |
| 5 | `fd_under_concurrency` | **驱动真实并发下载**：`run --parallel` + **64 个 task**（**`fetch` 路径并发度恒为 1，不能用** —— 只造 1 个 job，`buffer_unordered` 作用在 1 个任务上）。断言不因 `EMFILE` 失败；失败则错误信息须点明 fd 上限 | 5.3 |

> **第 5 条的修订理由（审计 F-05）**：原设计写的是「开 N 个文件直到失败，记录上限」——
> 那测的是**本机 fd 上限**（环境属性），与「工具在并发 64 时是否健壮」没有因果关系。
> 现改为驱动被测程序，判据落回 spec §5.3.2 的主体。
>
> 第 2 条的 `SKIP` 处理（审计 CL-03）：`cargo test` 无 skip 语义，打印 SKIP 后仍记 passed，
> 结果栏看起来就是通过。故在 CI 设 `REQUIRE_SPARSE=1`，此时 **SKIP 即 fail**；
> 本地环境不支持稀疏时允许 SKIP 并在输出里说明。

---

## 8. 定量指标（供 tasks 分批验收）

| 指标 | 目标 | 测量点 |
|---|---|---|
| 场景覆盖 | **14 条**（spec §5.1.1 + §5.2.1 + §5.4 逐条相加）| `run_all.sh` 计数 + `report.md` 行数 |
| 单场景耗时 | ≤ 120s（AIMD ≤ 300s），且实测 ≤ 场景元数据 `TIMEOUT_S` | 脚本 `SECONDS` |
| 可重复性 | 连跑 **3** 次结论一致 | `run_all.sh --repeat 3` |
| 环境残留 | 结束后 `tc qdisc show` 无 `netem` | 收尾断言 |
| 生产代码侵入 | ≤ 8 行、默认关闭 | `git diff src/` 统计 |

---

## 9. 前置阻塞与实测状态（2026-10-21 **全部已验**）

| 项 | 状态 | 证据 |
|---|---|---|
| **VM 可达** | ✅ SSH 直连 + MCP 均可用 | `ssh -i ~/.ssh/kali_vm_key kali@172.19.145.160` → `kali` / `7.1.5+kali-amd64`；`vm_bash` 亦通 |
| VM IP | ⚠️ **会飘** | 桥接校园网 DHCP（`172.19.145.160/17`）；连不上**直接问 lmq 要** |
| **sudo** | ✅ **限定三命令免密** | `/etc/sudoers.d/kali-netfaults`：`kali ALL=(ALL) NOPASSWD: /usr/sbin/tc, /usr/sbin/iptables, /usr/sbin/ip`；`visudo -c` parsed OK；对照：`sudo -n cat` 仍要密码 |
| **VM 内 Rust** | ❌ 未安装 | `cargo not found`；rustup 装 `~/.cargo` 不需 sudo，T2.3 自解 |
| Python | ✅ 3.14.8 | 假源可直接跑 |
| rootfs | ✅ **ext4** | `findmnt -no FSTYPE /`；稀疏文件语义可测 |
| `ulimit -n` | **1024** | 64 并发 × 4 分片 ≈ 320 fd，仍有余量 |
| `tc` | ✅ iproute2-7.2.0 + libbpf 1.7.0 | `tc -V` |
| **`netem mtu`** | ❌ **参数不存在** | 见 D2 rev3；改用 `ip link set dev lo mtu 1500`，**已实测可行** |
| `netem loss/reorder/duplicate/rate/delay` | ✅ 可挂载 | `tc qdisc add dev lo root netem loss 5%` 成功 |
| **`iptables --reject-with tcp-reset`** | ✅ 可建 **⚠️ 删除需完整 spec** | 见 D2 rev3 后半；按行号删亦可行 |
| 磁盘 | ✅ 51G 可用 | `df -h /home` |
| **假源 listen backlog** | ⚠️ **必须 ≥ 场景最大并发（取 128）** | stdlib 默认 5 会丢 SYN → RTO 重传 → 耗时差 7–8 倍。实测与取值依据见 §2.2 |
| **源端算力（GIL）** | ⚠️ **≥16 连的「吞吐 / 耗时 / 超时」类场景，必须先测源端单连基准** | ⚠️ **只写方法，不写阈值** —— 实测跨机器差 **20 倍**（同一份 `fakeorigin.py`：一台 `curl` 单连 375–383 Mbps，另一台 874–946 MB/s ≈ 7.5 Gbps）。写成常量就变成「换台机器就红」的新一类假判据。<br>做法：<br>`curl -s -o /dev/null -w '%{speed_download}\n' -H "Range: bytes=0-67108863" http://127.0.0.1:<port>/blob.bin`<br>N 连实测吞吐若明显低于 `N × 单连 × 0.7` ⇒ **先怀疑源端**（GIL / accept 队列），别记到客户端账上。<br>源端算力不足会把「源端慢」**伪装成「客户端慢」** —— 正是 D3 要排除的失真。<br>⚠️ **「正确性」类判据不受此限**（退出码 / 产物逐字节 / 无 EMFILE）—— 那些与源端快慢无关。<br>批次 3 最大只有 4 连 / 2 连，不受影响 |

**结论：批次 2 全部解除，L2/L2′ 可以开工。**
唯一遗留是 T2.3（VM 内装 Rust），不阻塞批次 3 之外的准备工作。

### 本次实测推翻/修正的两条假设（**这就是 design §9 存在的意义**）

1. **`netem` 支持 `mtu` 参数** —— 错。若不写进 §9 待验，会在批次 3 才炸，
   那时 7 个 L2 场景已按错误方式写完，返工面是整个批次。
2. **`iptables -D` 用原 spec 即可删除** —— 错（漏了 `--reject-with` 就删不掉）。
   若不发现，`clear` 会静默留残留规则，**污染后续所有场景**，且表现为「莫名其妙全部超时」。

---

## 10. rev2 修订记录（对应两道定稿门 findings）

| finding | 处理 |
|---|---|
| F-01 场景档位缩水 12→7 | **补足**：§2.1 列 14 个场景，与 spec 逐条对齐 |
| F-02 3 次 vs 2 次 | §8 明确 3 次；tasks T6.1 同步 |
| F-03 `report.md` 无载体 | §2.2 加 `emit_report`；§5 加 `run_all.sh` 负责生成 |
| F-04 `--repeat` 无定义 | §2.2 加 `run_all --repeat N` |
| F-05 `fd_budget` 测错对象 | §7 第 5 条改为驱动真实下载；删除段内自相矛盾的「断言 ≥256 / 不做硬断言」 |
| F-06 单位差 8 倍 | 全篇统一 **bit 制**；场景名带 `bit` 后缀 |
| F-07 禁公网无机制 | §2.2 加 `assert_local_only` |
| F-09 指数退避无判据 | spec 侧已降为描述性 |
| F-11 元数据缺 `assert` | §4.2 补 `# ASSERT:`，并作为 `run_all.sh` 的静态拦截 |
| F-12 128MB 档缺失 | 由 spec §5.2.1 明确用于乱序/重复；场景注释里写 `SIZE_MB: 128` |
| F-13 sha1 现算 | 新增 D8 + `expected_sha1.sh`（离线预算） |
| G-01 3 个接口无任务 | tasks T0.1 拆为骨架 + 核心两批 |
| G-03 ci.yml 行内矛盾 | §5 改为「确认，不改 / +0」 |
| G-08 §2.1 列 5 个 vs §2.2 列 7 个 | §2.1 不再列举，明确「唯一真相源是 §2.2」 |
| G-09 §1.2 缺 `run_all.sh` | §1.2 已补 |
| CL-01 「不改内部」冲突 | spec §5.4 加唯一授权例外，§3.1 回指 |
| CL-02 `layer: linux` 无载体 | spec §6.1 删除该枚举值 |
| CL-03 SKIP 算不算通过 | §7 第 2 条：CI 下 `REQUIRE_SPARSE=1`，SKIP 即 fail |
| CL-04 20s vs 10s | spec 统一为 20s |
| CL-05 `replay` 类不存在 | spec §7 删除该句 |

---

### rev3（2026-10-21 实测回填 —— **在 VM 上抓到两个真错**）

| 项 | 原假设 | 实测结果 | 处理 |
|---|---|---|---|
| `netem mtu 1500` | 支持按 MTU 切包 | ❌ **参数不存在**（iproute2-7.2.0 netem 无 `mtu`）| D2 改为「先 `ip link set dev lo mtu 1500`」，**已实测可行** |
| `iptables -D` 用原 spec 可删 | 是 | ❌ 漏 `--reject-with` → `Bad rule`，**规则残留** | `clear` 必须带完整 spec（或按行号）+ 删后自检 |
| sudo 需密码 | 阻塞 | — | 配限定三命令 NOPASSWD（`tc`/`iptables`/`ip`）|
| VM 内 Rust | 未知 | ❌ 未安装 | 转 T2.3，rustup 装 `~/.cargo` 不需 sudo |

**为什么写这一节**：这两条都是「我以为我知道」的 API 细节。
若不设 §9 的待验证清单，它们会在**批次 3 写完 7 个 L2 场景之后**才暴露——
那时返工面是整个批次，而不是改一行设计。
⇒ **`ssd-workflow` 五维预读里「已验证的外部实现」这一维，不能靠记忆充数。**
