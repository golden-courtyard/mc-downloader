# tasks — net_faults

> 2026-10-21 ｜ 基准：**`design.md` rev2**（同目录）
> 生成方式：从 design §5「改动点清单」+ §6「测试接缝」+ §9「前置阻塞」直接映射
> 排序原则：**依赖驱动，不是优先级驱动**
>
> **rev2 修订**：经第二道定稿门审计（10 条不一致），修补框架接口缺口、批次 4/5 的
> 输入/输出/验证命令、单位统一、验收总表回指错误；详见文末修订记录。

---

## 批次总览

| 批次 | 主题 | 依赖 | 可否并行 | 阻塞门 |
|---|---|---|---|---|
| **0a** | 框架骨架（`lib.sh` 全部接口）| 无 | 否（T0.1 是根）| 能跑通一个最小场景 |
| **0b** | 假源 + 期望值 + 诊断钩子 | 0a | 否 | 假源自测 sha1 一致 |
| **1** | Linux 平台语义（CI）| 无（与 0a 文件零重叠）| **可与 0a 并行** | T1.3 CI 绿 |
| **2** | VM 环境就绪 | 无 | 否（逐个验）| ✅ **T2.1/T2.2/T2.4 已通过**（2026-10-21），仅 T2.3 待做 |
| **3** | L2 传输层（7 个场景）| 0b、2 | 否（共用 `lib.sh`）| 7 条全 PASS |
| **4** | L2′ 内核层（6 个场景）| 0b、2、3 | 否（3 验证框架）| 6 条全 PASS 或记录已知偏差 |
| **5** | AIMD 验证（1 个场景）| 0b、2 | 否 | 阶跃曲线能归因 |
| **6** | 汇总与文档回填 | 1–5 | 否 | `run_all.sh --repeat 3` 三次一致 |

> **「0a 与 1 可并行」的依据**（审计要求逐文件核对，不只判逻辑独立）：
> 0a 触碰 `scripts/net_faults/lib.sh`；1 触碰 `tests/linux_semantics.rs` —— **交集 = ∅**。
> ⚠️ 但 0b 的 T0.6 会改 `src/download.rs`，若那时批次 1 的 CI 已跑过，
> **T1.3 的验收记录里要注明当时 `src/` 是否已含 T0.6**，否则将来无法回溯。

---

## 批次 0a — 框架骨架

### T0.1 `lib.sh` 骨架

- **输入**：design §2.2 接口清单（唯一真相源）
- **输出**：`scripts/net_faults/lib.sh`，实现 `load_env` / `clear` / `assert_sha` / `assert_rate`
- **要点**：
  - `clear` 必须**幂等**，且被 `trap ... EXIT` 调用（场景中途失败也要清 netem）
  - 带宽参数**一律 bps 整数**（`800000` / `8000000`），不接受 `8mbit` 这种字符串——
    单位换算只在一处做，避免各场景各写一遍
- **验证**：
  ```bash
  cd scripts/net_faults && bash -c 'source lib.sh; load_env; clear; assert_sha /etc/hostname "$(sha1sum /etc/hostname | cut -d" " -f1)"; echo ok'
  ```
  → 输出 `ok`，退出码 0。**`cd` 不能省** —— 在别的目录跑会 `lib.sh: No such file`

### T0.2 `lib.sh` 核心（**审计 G-01 补的**）

- **输入**：design §2.2 的 `start_origin` / `apply` / `run_case` / `assert_local_only`
- **输出**：上述 4 个接口实现
- **要点**：
  - `apply` 幂等：先 `qdisc del` 再 `add`，并把内核报错原样透出（别 `2>/dev/null` 吞掉）
  - `run_case` 收 `(name, 断言函数, 参数...)`，跑被测后把实测值传给断言函数
  - `assert_local_only` 用 `ss -tnp` 或 `/proc/<pid>/net/tcp` 核对对端，**全部 127.0.0.1 才算过**
- **验证**：跑通一个**最小场景**（不施加故障，直接下 4MB）→ 打印 `PASS smoke sha=ok`

### T0.3 `lib.sh` 收尾自检 + **反向用例**

- **输入**：T0.1 + T0.2
- **输出**：`lib.sh` 末尾的 `trap` 与 `self_check` 函数
- **验证（正例）**：
  ```bash
  cd scripts/net_faults && bash -c 'source lib.sh; load_env; self_check' && echo SELFTEST-OK
  ```
- **验证（反例，`cd` 同样不能省）** —— 这三条是 0a 审查抓出的假 PASS 的回归判据，**缺一条就等于没修**：

  | # | 反例 | 期望 |
  |---|---|---|
  | 1 | 先造一个 sha 正确的 `CASE_OUT`，再让被测程序**立刻失败**（`BIN=/usr/bin/false`） | 必须打 `FAIL`，**不得**因残留文件判 `PASS` |
  | 2 | `CASE_PEERS` 为空 + 进程已退出 | `assert_local_only` 必须返回**非 0**（`no_evidence`）|
  | 3 | 先 `iptables -A OUTPUT -p tcp --dport 22 -j ACCEPT`，再 `clear` | `clear` 必须仍返回 **0**（判据只数我们自己那条 `--sport $PORT`，不要求链里只剩表头）|

- **收尾**：`ssh kali@<VM> "tc qdisc show dev lo | grep -c netem"` → 期望 0

---

## 批次 0b — 假源 + 期望值 + 诊断钩子

### T0.4 `expected_sha1.sh`（**审计 F-13 补的**）

- **输入**：design D8（离线预算）
- **输出**：脚本 + 一份 `expected_sha1.txt`，含 16MB / 128MB / 1GB 三档的 sha1 常量
- **要点**：内容规则 **`bytes(i % 251)`，`i` 是全局字节偏移** —— 天然周期 251，**没有"循环长度"这一说**
  （原写的"循环长度 16"是笔误，已删）。与 `fakeorigin.py` 必须用**同一个表达式**，不是"两边各算一遍"
- **产物必须入库**：`expected_sha1.txt` 要进仓库。理由（D8）：
  若每次跑场景才现生成，"生成脚本自己写错"与"常量"会**同时变**，离线比对的独立性就丢了
- **验证**：
  ```bash
  bash expected_sha1.sh 16   # 独立于 fakeorigin 算出
  # 再让 fakeorigin 生成同尺寸，比对两者 sha1 —— 一致才算对
  ```

### T0.5 `fakeorigin.py`

- **输入**：design D3（**不限速**）、D8（期望值来自场景常量，不由本源现算）、**design §2.2 的 CLI 约定**
- **输出**：支持 `HEAD` + `Range`(206) 的源；启动打印 `SIZE=<bytes>`
- **CLI（已由 0a 锁定，不许改）**：`python3 fakeorigin.py --size-mb N --port P [--verify SHA]`
  —— `lib.sh` 的 `start_origin` 已按此调用，改名会让 14 个场景全部失败
- **三点硬要求**：
  1. **必须 `allow_reuse_address = True`** —— 否则 TIME_WAIT 会让 `bind()` 失败，
     表现为「假源启动即退出」，且 `ss -tan` 只见 TIME-WAIT 不见 LISTEN，极易误判成残留进程
  2. **不得内置 sleep 限速**（限速只由 netem 施加，D3）
  3. `--verify <sha1>` 给出时自校验生成结果，不符即 `exit 1`（D8 的落地）
- **验证（三步，缺一步就是「验过了但没验到」）**：
  ```bash
  # ① 制造服务端 TIME_WAIT —— 必须用 Connection: close 让**服务端先关**。
  #    否则 TIME_WAIT 落在客户端临时端口，这条用例根本触发不到目标缺陷。
  python3 fakeorigin.py --size-mb 1 --port 8899 & OPID=$!
  sleep 1
  curl -s -H 'Connection: close' -o /dev/null http://127.0.0.1:8899/blob.bin
  kill "$OPID"; wait "$OPID" 2>/dev/null
  ss -tan | grep -c ':8899.*TIME-WAIT'        # 期望 ≥1

  # ② 对照：不开 SO_REUSEADDR 的裸 socket 必须 bind 失败
  python3 -c "import socket; socket.socket().bind(('127.0.0.1',8899))"
  # 期望 [Errno 98] Address already in use —— 证明 SO_REUSEADDR 是承重的，不是摆设

  # ③ 第二轮必须成功。⚠️ 不要写 `... && echo REUSE-OK`：
  #    服务是常驻 serve_forever()，`&&` 后面的命令**永远不执行**
  python3 fakeorigin.py --size-mb 1 --port 8899 & OPID=$!
  for _ in $(seq 50); do (echo >/dev/tcp/127.0.0.1/8899) 2>/dev/null && break; sleep 0.1; done
  kill -0 "$OPID" && echo REUSE-OK
  curl -r 0-99 -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8899/blob.bin   # 206
  kill "$OPID"
  ```
  ⚠️ 两个环境坑：**不要用裸 `wait`**（会连常驻假源一起等，ssh 挂死）；**不要用 `/usr/bin/time`**（VM 上没有，只有 shell 内建 `time`，计时用 `date +%s%N`）。

### T0.6 假源自测

- **输入**：T0.4 + T0.5
- **输出**：本机（或 VM）跑 `mc-downloader fetch` 落盘，sha1 与 T0.4 常量一致
- **验证**：
  ```bash
  python3 scripts/net_faults/fakeorigin.py --size-mb 16 &
  ./mc-downloader --piece-conns 2 fetch http://127.0.0.1:8899/blob.bin --dir /tmp/t --name t.bin
  sha1sum /tmp/t/t.bin     # 与 expected_sha1.txt 的 16MB 行一致
  ```
- **判据**：**这是后续所有场景的地基**——假源错了，后面全错

### T0.7 `MC_TUNE_LOG` 钩子（唯一生产代码改动）

- **输入**：design §1.3 D6 的三条约束
- **输出**：`src/download.rs` tuner 循环内 +8 行，只 `eprintln!`，**不参与任何判断**
- **验证**：
  ```bash
  ./mc-downloader ... 2>&1 | grep -c "\[tune\]"              # 期望 0（默认关闭）
  MC_TUNE_LOG=1 ./mc-downloader ... 2>&1 | grep -c "\[tune\]" # 期望 >0
  ```
- **回归断言**：`git diff src/` 只应有这 8 行；`cargo test` 仍 62 passed；`clippy -D warnings` 仍绿

---

## 批次 1 — Linux 平台语义（CI，不需 root）

### T1.1 `tests/linux_semantics.rs`

- **输入**：design §7 的**五条**判据
- **输出**：5 个测试；**用文件级 `#![cfg(target_os = "linux")]`** —— 逐项挂 `#[cfg]` 会让 Windows 上
  `unused_imports` 撞 CI 的 `-D warnings`，文件级更干净，效果等价
- **要点**：
  - **不打桩**，落真实文件系统（design §6）
  - 第 2 条 `sparse_set_len`：设 `REQUIRE_SPARSE=1` 时 SKIP 即 fail。**CI 已在 Test 步设该变量**（ci.yml）
  - 第 5 条 `fd_under_concurrency`：**驱动真实并发下载** —— 必须 `run --parallel` + 64 个 task。
    断言只有三类：**退出码 0 · 无 EMFILE · 64 个产物逐字节一致**。
    ⚠️ **连接数不是判据** —— 「64 个任务」只是驱动条件（压力来源）：
    - **不能用 `fetch`**：只造 1 个 job，`buffer_unordered` 作用在 1 个任务上，并发度恒为 1，**压力为零**
    - **不能用单文件 `--piece-conns 64`**：实测同机器同版本，1GB×64 会 **44↔64 波动**
      （天花板不是 accept 队列 —— 队列已够；是「worker 启动速度 vs 单片完成速度」的竞赛：
      传得太快，后启动的 worker 还没连上，前面的片已经下完了）
      ⇒ **别把峰值写成断言**，否则随机红
    - ⚠️ **本条的强度上限是「驱动到位」，不是「实证到位」**：
      峰值浮动只让实际压力变小，**不会造成假红**；但也无法证明"至少 N 条在飞"。
      想实证就得在测试里读 `/proc/net/tcp` 数连接 —— **不建议**，那是把刚清掉的易碎判据请回来

- ⚠️⚠️ **两阶段的分工必须写清，否则会被误读成"阶段 A 也在测并发"**：

  | 阶段 | 命令 | 实际连接数 | 它证明什么 |
  |---|---|---|---|
  | **A** | `fetch --concurrency 64`（1 MiB 单流）| **1 条**（<8MB 不分片 ⇒ 1 片 1 连）| spec 原文那条命令**没崩**（兼容性检查）|
  | **B** | `run --parallel` 64 个 task | **64 条** | **fd 健壮性**（真正的压力点）|

  ⇒ **阶段 A 是兼容性检查，阶段 B 才是压力点。**
  ⇒ 若将来有人把**阶段 B 的规模调小或删掉**，**阶段 A 会留下一张"看起来覆盖了并发"的假绿**
  —— 这正是本 feature 花三轮在修的那类病。
- **验证**：`cargo test --test linux_semantics` → Linux 上 5 passed
- ⚠️ **别在注释里写「Windows 上这几条不成立」**：实测 NTFS 上 `rename` 覆盖已存在文件、
  「删除打开中的文件」**都能过**，真正不成立的只有**大小写敏感**（判据 1，实测建 `A`/`a` 后目录只剩 1 条）。
  `cfg(linux)` 的作用是**守住 Linux 侧语义判据**，不是"Windows 会失败"。

### T1.2 本地确认分流

- **验证**：Windows 上 `cargo test --test linux_semantics 2>&1 | grep "running 0 tests"`
  （是 0 个用例，**不是**失败、**不是** skip）

### T1.3 CI 绿

- **输入**：T1.1 推送
- **输出**：GitHub Actions 的 `Test` 步包含这 5 个用例且全过
- **核对动作**：确认 `.github/workflows/ci.yml` 的 `--all-targets` 已把 `tests/` 纳入 → **无需改 ci.yml**（design §5 已定）
- **验证**：`gh run view --log | grep linux_semantics`
- **阻塞**：若某条失败，**先判是「我们错了」还是「环境如此」**，别改断言迁就

---

## 批次 2 — VM 环境就绪（**L2/L2′ 的硬前置**）

### T2.1 连通性 —— ✅ **已完成**

- **结果**：**两条通道都可用** —— SSH 直连（`kali@172.19.145.160`）+ MCP `vm_bash`（重启后读到新 IP）
- **纪律**：SSH 直连作为**备用通道**保留 —— MCP 是启动时读死 IP 的，
  下次 IP 再飘就先 ssh 试，别为一个 MCP 重连去重启整个会话
- **注意**：IP 是桥接 DHCP，**会飘**；连不上先问 lmq 要当前 IP

### T2.2 解除 sudo 阻塞 —— ✅ **已完成（2026-10-21 实测）**

- **结果**：写入 `/etc/sudoers.d/kali-netfaults`，**限定三命令**：
  ```
  kali ALL=(ALL) NOPASSWD: /usr/sbin/tc, /usr/sbin/iptables, /usr/sbin/ip
  ```
- **验证**：`visudo -c -f` → `parsed OK`；`sudo -n tc/iptables/ip` 三条均通；
  **对照**：`sudo -n cat /etc/hostname` 仍要密码（证明是限定而非全量）
- **纪律**：**不要换成全量 NOPASSWD** —— 该 VM 是桥接校园网，暴露面比 host-only 大，
  只放开测试真正需要的三条

### T2.3 VM 内可编译 —— ✅ **已完成（2026-10-21）**

- **结果**：`rustc` / `cargo` **1.99.1**；`cargo build --locked --release` 通过
  （首次 405s 含 207 个 crate 下载，依赖缓存后 **28.34s**）；最小下载 sha1 与常量一致
- **代码传输**：`tar -czf - src Cargo.toml Cargo.lock … | ssh … 'tar -xzf -'`
  —— 构造上就排除 `.git/` 与 `target/`，且**字节级传输不经过文本转换**（行尾天然不变）。
  到 VM 后逐文件核对：18 个文件 CR 字节全 0，`Cargo.lock` 两侧 sha1 相同。
- ⚠️ **漏掉的一步（D-21）：编译需要 OpenSSL 头文件**
  `reqwest 0.12` 默认 features → `native-tls` → `openssl-sys`；
  VM 只有运行时 `libssl.so.3`，**无头文件、无 pkg-config**，而 `apt-get install` 要密码。
  免 root 解法：
  ```bash
  apt-get download libssl-dev            # 走镜像；版本要与已装的 libssl3t64 对齐
  dpkg-deb -x libssl-dev_*.deb ~/openssl-dev
  # 私有 prefix：include 指向解出的 140 个头
  #   ⚠️ 架构特定的 opensslconf.h / configuration.h 在 include/x86_64-linux-gnu/openssl/，不在 include/openssl/
  #   lib 指向系统 libssl.so.3 / libcrypto.so.3（头只在编译期需要）
  ```
  然后固化到 **VM 的** `~/.cargo/config.toml` 的 `[env] OPENSSL_DIR / OPENSSL_NO_VENDOR`，
  使后续 `cargo build` 无需任何环境变量。
  **仓库的 `Cargo.lock` 一个字节没动**（两侧 sha1 相同）。
- ⚠️ **VM 重装后这一步必须重做**（`/etc` 和 `~` 都在 VM 盘上）
- **验证**：`--version` → `mc-downloader 0.5.0`；最小下载 sha1 = `bd60405be79948989a6c1d6963ee20e6c940b936`
- ⚠️ **不要用「VM 上 `git status`」做洁净度判据**：tar 传过去时排除了 `.git`，VM 上不是仓库。
  正确判据是**两侧 `Cargo.lock` 指纹相等 + 本机 `git status` 为空** ——
  前者证明"传过去的一致"，后者证明"没被改动"，比单看 `git status` 更直接。

### T2.4 netem / iptables 能力探测 —— ✅ **已完成，结论推翻了一处设计**

- **结果**：

  | 探测项 | 结论 |
  |---|---|
  | `netem rate` / `delay` / `loss` / `reorder` / `duplicate` | ✅ 均可挂载 |
  | **`netem mtu 1500`** | ❌ **参数不存在**（iproute2-7.2.0 的 netem 无 `mtu`）|
  | `ip link set dev lo mtu 1500` | ✅ 可行，随后 netem 正常挂载，MTU 可恢复 |
  | `iptables -A ... -j REJECT --reject-with tcp-reset` | ✅ 可建 |
  | `iptables -D`（**漏** `--reject-with`）| ❌ `Bad rule`，**规则残留** |
  | `iptables -D`（完整 spec 或按行号）| ✅ 可删 |

- **对 design 的影响**：D2 已改为「先 `ip link set dev lo mtu 1500`」，见 design rev3（含两项实捕记录）
- **对 `lib.sh` 的硬要求**：
  - `apply` 前设 MTU 1500，`clear` 还原 65536（**成对管理**，漏了会让后续场景全在 1500 下跑）
  - `clear` 删 iptables **必须带完整 spec**，删完自检 `iptables -L OUTPUT -n` 只剩表头
- **遗留待验（转 T3.1）**：改 MTU 后 netem 是否**真按 1500 粒度算包**（`lo` 上仍可能有 GSO 合并）。
  判据：同 loss 率下 MTU 1500 与 65536 的耗时应有显著差异；无差异 ⇒ 转 veth pair

---

## 批次 3 — L2 传输层（7 个场景）

> 前提：T0.6、T2.3、T2.4 全通过

### T3.1 `rate` 四档 —— ✅ **已完成（7/7 PASS）**

- **实测判定行**：
  ```
  PASS l2_rate_800kbit rc=0 rate=748498    expect=800000    tol=0.2 sha=ok   90s
  PASS l2_rate_8mbit   rc=0 rate=7588925   expect=8000000   tol=0.2 sha=ok   18s
  PASS l2_rate_80mbit  rc=0 rate=72121293  expect=80000000  tol=0.2 sha=ok    2s
  PASS l2_rate_400mbit rc=0 rate=370000628 expect=400000000 tol=0.2 sha=ok    3s
  ```
  换算 93.6% · 94.9% · 90.2% · 92.5%，全部落在 ±20% 内。
- ⚠️ **`400mbit` 档必须用 128MB，不是 16MB**（实测修正）：
  16MB 的观测窗口只有 ~400ms，其中约 90ms 是进程启动 + 长度探测 = **20% 开销**
  ⇒ 实测会抖到 71.7%（卡下界）。128MB 后稳定 366–370 Mbps。
  ⇒ ⭐ **原则：判据的观测窗口必须远大于被测程序的固定开销**，否则测到的是开销不是带宽。
- ✅ **`8MB 真的 2 片`**（直接证据，非推断）：跑动中直读 `.part.state` 头部 ——
  `8MB → total=2 piece_size=4MB` · `16MB → total=4` · `128MB → total=32`
- **`800kbit` 档用 8MB**：16MB / 800kbit = 160s，超 `timeout_s ≤ 120`；8MB 正好 80s。

### T3.2 `delay` 两档 —— ✅ **已完成**

```
PASS l2_delay_20ms  rc=0 elapsed=2086ms  expect>=20ms  sha=ok    2s
PASS l2_delay_200ms rc=0 elapsed=10870ms expect>=200ms sha=ok   11s
```

- ⚠️ **200ms 档实测 10.9–11.5s，不是天真估计的 0.6s**：`netem delay` 在 `lo` 上**对两个方向都生效**
  （RTT ≈ 400ms），加 TCP 慢启动，16MB / 4 连要 ~57 个 RTT。
  这个数吃掉 spec 120s 预算的 1/10 —— **批次 5 的 AIMD 阶梯（每段 25s）要注意**。
- ⚠️ **下界判据太弱**：20ms 档下界只有 20ms（实际 2086ms）、200ms 档下界 200ms（实际 10.9s），
  只能拦住"delay 完全没生效"这类装配错误。
  ⇒ **建议改成 `总耗时 ≥ delay × 2`（一个 RTT 往返）**，比 `delay × 分片数 ÷ 并发` 更贴物理。

### T3.3 中途 RST —— ✅ **已完成（但判据②未触发，见下）**

```
PASS l2_rst_mid rc=0 recovered=yes rst_at=1707ms rst_kb=8192 elapsed=18757ms sha=ok   19s
```

- ⚠️ **两个参数必须改**（原设计"无 netem + 16MB + 4 连"**三者一起不可执行**）：
  1. **加 `rate 50mbit`**：不加 netem 时 16MB 只 130ms，50% 在 65ms 处，
     而 iptables 加规则往返 10–30ms ⇒ "打到一半"打不准，注入晚了等于没测。
     加后窗口 2.6s，实测注入点 1454–1707ms。
  2. **`--piece-conns` 4 → 2**：4 连时 4 个 4MB 片**对称完成**，`.part` 直到最后一刻才落盘
     （实测 `du` 在 1s 内全程 0，而日志已显示 5.7MB）⇒ 判据②「`.part.state` 分片数 > 0」**永不成立**。
     2 连时按序完成，50% 时已有 2 片落盘（`du=8192KB`）。
- ⚠️ **判据②（报错退出 → state>0 → 下次能续上）本轮没走到**：两次都恢复了。
  归因实验（RST 保持 0.6s / 6s / **40s**）—— **40s 仍 recovered=yes**（44.9s 完成）。
  源码依据：`download.rs:175` 与 `:394` **两层重试**，`retries=3`，退避 `1<<attempt`（1+2+4s）
  ⇒ 单分片约 7s，整包 3 轮，**总预算 28–40s**。
  ⇒ **结论**：要触发判据②需 RST ≥ 60s 且 `TIMEOUT_S=120`（约 100s/次）。
  **本 feature 不作为常规场景**——真实使用中"重试耗尽"极罕见，且它是设计意图之外的路径。
  但**必须知道这个数**：批次 4 的 `l2p_loss_20` 若把连接全打死，失败前会先耗 30–40s 重试。

---

## 批次 4 — L2′ 内核层（6 个场景）

> 前提：批次 3 证明框架可用
> **全部使用 128MB 测试文件**（`SIZE_MB: 128`）——分片数不足时偏移错位类缺陷不显现
>
> ⚠️ **必须显式 `--piece-conns 32`**：128MB 有 32 片，而默认 `piece-conns` 是 **4**。
> 不显式给就只有 4 条连接在写，「偏移错位」这类缺陷根本测不到。
>
> ⚠️ **期望并发按实测推，别按名义 32 反推**：修 backlog 后 128MB 档实测峰值 **28–32**；
> 修之前只有 11–15。详见「场景编写须知」里的实测表。

### T4.1 `loss` 三档 —— ✅ **已完成**

```
PASS l2p_loss_1  rc=0   sha=ok                                                      1156ms
PASS l2p_loss_5  rc=0   sha=ok                                                      3870ms
PASS l2p_loss_20 rc=124 outcome=timeout_killed part_state_done=2 resumed=resume sha=ok  91201ms
```

- **`loss_20` 走的是「超时被杀」**（rc=124，90s 内没有一片 4MB 下完）。
  单独探过：**200s 内一片都没完成**（`.part` 分配 0MB、state done=0）
  ⇒ 20% 丢包下 32 条 TCP 会拥塞崩溃，"完成"这条路在本档基本不存在。
- ✅ **三态区分**（这是本条的核心判据）：

  | 态 | 特征 | 判定表达 |
  |---|---|---|
  | 完成 | rc=0 | `outcome=completed` + sha |
  | **装置杀掉** | `timeout 90` 返回 **124** | `outcome=timeout_killed` |
  | 程序自己放弃 | 非 0 且非 124（两层重试耗尽，28–40s）| `outcome=errored` |

- ⭐ **"不得留假象"是用「真的续一次」验的，不是看文件在不在**：
  撤 netem、保假源活着、同目录再跑 —— 续得上且 sha1 正确 ⇒ state 诚实；否则判 `sha=假象(...)`。
  两种进度都实测过：`done=2` → `resumed=resume`（真接着下）；`done=0` → `restarted`（0 进度不冒充进度）。

- ⚠️ **框架与契约冲突（待 lib.sh 解冻时修）**：
  `run_case` 的硬规则是「只要 `CASE_TIMEOUT==1` 就判 FAIL，与断言结果无关」，
  而本档的契约**允许超时** ⇒ 判定行写 `sha=ok` 最终仍是 FAIL。
  当前变通：`BIN="timeout"`，把装置超时下移成普通非零退出 `rc=124`，`CASE_TIMEOUT` 保持 0。
  副作用：`CASE_PID` 变成 `timeout` 的 pid，**本档不能用 `assert_local_only`**（本档不需要）。
  ⇒ 建议加 `CASE_TIMEOUT_OK=1`，让"超时属于可接受结果"的场景不必用外层 wrapper。

### T4.2 `reorder` 两档 —— ✅ **已完成，但判据偏弱**

```
PASS l2p_reorder_10 rc=0 sha=ok   3241ms
PASS l2p_reorder_25 rc=0 sha=ok   4746ms
```

⚠️ **现有判据（只查 sha1）指向偏了**：
`reorder` 只是让**段**乱序，TCP 会在内核里重排，**正常情况下本就不该影响 sha1**。
它真正要暴露的是「我们并发写同一偏移」的竞态，而这个竞态在 10%/25% 下都稳定通过。

⇒ **结果：它和 `l2p_loss_1` 的判据没有区别 —— 看起来覆盖了两件事，实际只覆盖一件。**
⇒ **建议**（下一轮改）：判据补一条「确有乱序/重传的实际证据」，例如 `ss -ti` 的 `retrans` 增量或设备侧计数。

### T4.3 `duplicate` —— ✅ **已完成**

```
PASS l2p_dup_10 rc=0 sha=ok   1048ms
```

判据 sha1 一致（TCP 应去重；不一致说明我们自己的 `expect` 长度校验有问题）。

### 批次 4 的 `TIMEOUT_S` 实测

| 场景 | 实测 | 设定 | 余量 |
|---|---|---|---|
| 除 `loss_20` 外五个 | 1.0–4.7s | 60 | 很宽 |
| **`loss_20`** | **91s** | 120（内层 90）| 只剩 1/4 |

⚠️ `loss_20` 的 91s 已吃掉 spec「单场景 ≤120s」的 **3/4**。
⇒ 若以后把丢包提到 30%，必须换更小的文件，否则**必然超限**。

---

## 批次 5 — AIMD 验证（1 个场景）

### T5.1 阶跃脚本 —— ⚠️ **素材尺寸必须按 2s tick 倒推，不能按 MB 数想当然**

- **序列**：`80mbit → 8mbit → 80mbit`，每段 **25s**（共 75s）
- ⚠️⚠️ **tuner 是 `interval(2s)` + `delta == 0` 就 `continue`**
  ⇒ 传输必须在 75s 内**持续推进**，否则**一行 `[tune]` 都不会有**，等于没测。
  ⇒ 实测（无限速本机）：16MB 40ms / 1GB **2.5s** ⇒ 两者都出 **0 行**；4GB 才有 3–5 行。
- **素材尺寸算法**（每段传输量 = 段时长 × 该段带宽）：

  | 段 | 带宽 | 25s 内传输 |
  |---|---|---|
  | 1 | 80mbit (10 MB/s) | 250 MB |
  | 2 | 8mbit (1 MB/s) | 25 MB |
  | 3 | 80mbit (10 MB/s) | 250 MB |
  | | **合计** | **525 MB** |

  ⇒ **1GB 文件够用**（三段走完还剩 ~475MB，可继续跑完或提前收工）。
  ⚠️ 先确认 `fakeorigin.py` 的 1GB 是**流式生成**还是预生成 —— 预生成会吃 1GB 内存。
- ⚠️ **`--adaptive` 必须写 `--adaptive=true`**：`main.rs:35` 是 `Option<bool>`，不带值会 `exit=2`。
- **产物** `aimd_trace.csv`，两类事件混在一张表里按时间排序：

  ```
  t_ms, evt, tput_bps, act, delta, last, bytes
  ```
  - `evt` ∈ {`tick`, `tune`}
  - `tput_bps` 只在 `tick` 行有值；`act`/`delta`/`last` 只在 `tune` 行有值

  ⚠️ **不要用 `avail` 列**：饱和状态下 `add_permits(1)` 后阻塞的 worker 立刻抢走，
  读回来恒为 0，**看起来像"tuner 没动"（假阴性）**。用 `act`（`add`/`cut`/`hold`）。
- **验证**：`bash scenarios/aimd_step.sh && head -3 /tmp/aimd_trace.csv`

### T5.2 跑并归因 —— ✅ **已完成（PASS，三处判据按实测修正）**

```
PASS aimd_step A:cut=11 A:add=14 A:seg2=6854846 A:seg3=73810765
     B:add=22 B:cut=4 B:seg2=5908793 B:zero2=4/23 B:seg3=73411291
     C:tune=0 C:seg2=6930963 C:max=94046454(诊断)
```
墙钟 234s（三臂 × 78s）。产物 `$OUT/logs/aimd_{a,b,c}.csv`。

| 臂 | 参数 | 命题 | 实测 |
|---|---|---|---|
| **A** | `--piece-conns 1` | AIMD 双向有效 | cut=9–11 · add=14 · seg2 86–101% · seg3 73–80M |
| **B** | `--piece-conns 16` | 高并发回归 | add=18–22 · **cut=1–4** · seg2 5.9–7.4M · **zero2=1–4/23** |
| **C** | 16 连 + `--adaptive=false` | 对照 | **tune=0** ✓ · seg2=6.93M |

⚠️ **`[tune]` 在 `delta == 0` 时静默**（`continue` 在打印之前），所以**丢包/重试的 30–40s 里本来就该一行都没有**。
用它判"tuner 停没停"会得到**反的结论** —— 与批次 4 那条「别把还在重试记成失败」同族：**拿缺失当证据**。
判"没动"看的是**整段内 `add`/`cut` 一次都没出现**，不是"全是 hold"。

#### ⚠️ 三处判据修正 —— **每条都附「原判据为什么不能用」**

| 修正 | 原判据 | **死因（实测证据）** |
|---|---|---|
| B 去掉 `seg2 ∈ ±20%` | 用**均值**判 8mbit 段收敛 | 16 连在 8mbit 段是**双峰**：`7.5→4.2→0→0→2.5→7.5`，**4/23 tick 为零吞吐**。均值 5.9M 是双峰的平均 ⇒ **用均值判抖动现象，测的不是它** |
| C 去掉 `max ≤ 16×8mbit` | 用 **max** 判吞吐上限 | `max=194Mbps` 在 t=64.4s，**前 5 个 tick 为 0** —— 停摆后恢复时把积压一次性送出（24MB）的**回补脉冲**，不是持续速率 ⇒ **max 判持续速率 = 测错对象** |
| B 的 `cut` 由「预期 0」转诊断 | 基于 25s 窗口的算术推断 | **78s 窗口实测 cut=1–4**；25s 窗口是 0。同一份代码 ⇒ **窗口长度决定结论** |

**共同根因：统计量选错了对象**（均值 vs 抖动 · max vs 持续速率 · 窗口 vs 稳态），**不是阈值宽严**。

#### 两个现象：**不是产品缺陷，但永远不能作吞吐判据的测量点**

| 现象 | 数据 | 定性 |
|---|---|---|
| **16 连 + 8mbit 周期性停摆** | 8mbit 窗口 23 tick 里 4 个为 0；换挡后连续 5 个为 0 | **① 不是产品缺陷**（换 80mbit 不出现）<br>**② 也不是纯 netem 伪影**：8mbit=1MB/s、16 流各 ~62KB/s、netem 队列 `limit 1000` 包 ≈1.5MB≈1.5s 容量 ⇒ 拥塞崩溃→停摆→积压→涌出。**真实窄带链路（校园网/移动网）同样这个形状** |
| 停摆后 194Mbps 回补 | t=64.4s 单 tick 24MB | 同上，恢复期送出积压 |

⇒ **纪律：测高并发吞吐档位必须 ≥80mbit；「16 连 + 8mbit」只能测"低带宽下能否完成"，不能测速率。**

#### ⭐ 本批得出的一条新纪律

> **改判据时，必须同时记录「被去掉的那条判据 + 它为什么会误判的实测证据」。**
> 只写新判据、不写旧判据的死因，等于把知识丢掉 ——
> 下一个人看到 `seg2=5.9M` 会以为产品坏了，而不是知道"这是双峰的平均"。

#### 两处实现说明

- **本场景不走 `run_case`**：它只跑一条命令，而这里要编排三臂。三臂跑完后直接 `__nf_verdict` 出**一行**，保持「14 场景 14 行」（`emit_report` / `run_all` 的计数不受影响）。
- **每臂 78s 由 `timeout` 收工**（1GB 在 75s 内下不完是预期：理论 525MB / 素材 1000MB），每臂独立目录，不需要为下一臂清 `.part`。

---

## 批次 6 — 汇总与文档回填

### T6.1 `run_all.sh` 与报告 —— ✅ **已完成（`b5fe2be`，239 行）**

- **全量实跑**：`TOTAL 14/14 passed · repeat=3 · 挂钟 1444s`，14 行全部 `ok(3轮一致)`
- 慢场景均在限内：aimd 235s/轮（≤300）· loss_20 91s（≤120）· 800kbit 90s（≤120）
- 三处残留自检干净：`noqueue` / iptables 2 行 / `mtu 65536` / 无残留进程

**八件事的落点**：恰好 14 个否则退出 · `# ASSERT:` 缺失则不执行 · `--repeat N` 只比结论不比数值 ·
`--only` 过滤但**场景总数仍校验 14** · `report.md` 七列 · 残留三处 · 耗时断言 + `timeout TIMEOUT_S+60` 兜底 · 汇总行

#### ⚠️ 报告行数的两个口径（别再混）

| 口径 | 值 | 说明 |
|---|---|---|
| `wc -l` | **16** | markdown 表必须有 `|---|` 分隔行 ⇒ 表头占 2 行 + 14 数据 |
| `grep -c '^| '` | **15** | 只数以 `\| ` 开头的行 ⇒ 表头 1 + 14 数据 |

⇒ **验收判据用 `grep -c '^| '` = 15**；用 `wc -l` 会恒差 1，且删掉分隔行表就不渲染了。

#### ⚠️ 两个让报告「静默错位」的坑（实现时踩过，已修）

1. **多个并行数组必须同长**：用 `+=` 追加时若给 4 个数组各追加两次，报告会**整体错位一行**
   （第一行耗时显示 `-`、第二行显示别人的值）。改为按 `idx` 写入。
2. **bash 5.3 + `set -u` 下 `declare -a X`（不带 `=()`）取 `${#X[@]}` 报 unbound**（实测 `rc=1`）。
   必须写 `declare -a X=()`。

⇒ 这两条的共性是：**报告错位不报错**，和本 feature 一直在防的「静默失效」同类。

#### ⚠️ 一条给未来的警告：`B:cut` 不许升级回判据

实测三次独立运行 `B:cut` = **0 / 4 / 0**，`B:zero2` = 4 / 7 / 23。
它**随 add 次数浮动**（见 DESIGN §8.2 的窗口公式），作诊断项合理；
**一旦有人把它写成断言，就会变成随机红**。

#### ⚠️ 一条工具教训：`strings` 查二进制字符串不可靠

曾想用 `strings target/release/mc-downloader | grep -c MC_TUNE_LOG` 做前置检查。
实测 **`strings` 与 `strings -a` 都返回 0**，而程序跑起来正常出 4 行 `[tune]`：

```
偏移 0x247f4e: 4d43 5f54 554e 455f 48   ← "MC_TUNE_H"，后面就是机器码
完整串 "MC_TUNE_LOG" 逐字节找：0 次
```

⇒ **编译器把 11 字节字面量拆成立即数写进指令**，二进制里没有连续字符串。
⇒ **前置检查只能功能性**：跑一次 `MC_TUNE_LOG=1 … --adaptive=true` 看有没有 `[tune]`。

### T6.2 `DESIGN.md` 回填

- **输出**：§8.2 把「AIMD 功效验证」标为已落地或记录结论；§11 补「三层验证能力」的实测结果
- **验证**：`grep -n "net_faults" DESIGN.md` 有命中
- **注意**：回填时若引用「场景数」，**必须与 spec §5 的 14 条对得上**（审计 F-01 的连带风险）

---

## 验收总表

> 左列注明来源文档，避免把 `design` 的节号标成 `spec` 的（审计 G-06）。

| 来源 | 条目 | 覆盖任务 | 状态 |
|---|---|---|---|
| spec §5.1（L2）| rate×4 / delay×2 / RST×1 | T3.1 T3.2 T3.3 | [ ] |
| spec §5.2（L2′）| loss×3 / reorder×2 / dup×1 | T4.1 T4.2 T4.3 | [ ] |
| spec §5.3（平台语义）| 5 条 | T1.1 | [ ] |
| spec §5.4（AIMD）| 阶跃 + 对照 | T5.1 T5.2 | [ ] |
| spec §5.5（可判定）| 一行判定 + 禁无断言 | T0.2 T4.2 的 `ASSERT` 校验 T6.1 | [ ] |
| spec §4.1（耗时）| ≤120s / ≤300s | T6.1 耗时断言 | [ ] |
| spec §4.2（可重复）| **3 次**一致 | T6.1 `--repeat 3` | [ ] |
| spec §4.3（禁公网）| 对端全 127.0.0.1 | T0.2 `assert_local_only` | [ ] |
| spec §4.5（分层）| 语义走 CI、L2 不进 CI | T1.3 | [ ] |
| spec §6.2（数据）| 非均匀 + **三档尺寸** + 离线 sha1 | T0.4 T0.6 + 批次 4 的 `SIZE_MB:128` | [ ] |
| **design §9**（前置阻塞）| sudo / Rust / netem-mtu | T2.1 T2.2 T2.3 T2.4 | [ ] |

---

## 场景编写须知（批次 3 起必读）

`lib.sh` 经两轮审查 + 两轮修复后的**对外行为约定**。写场景前先看这节，能省掉一轮返工。

### 三处行为变化（0a 修复引入，**不是 bug**）

| 变化 | 现在的行为 | 对场景的影响 |
|---|---|---|
| **`clear` 未 arm** | 返回 **2**，且**不碰任何设备** | 场景必须先 `load_env`。裸调 `bash -c 'source lib.sh; clear'` 返回 2 是「无事可清」，不是失败 |
| **`assert_local_only <pid>`** | **只认 `CASE_PID` 的采样证据**；问别的 pid 一律现场 `ss` 采 | 想复用采样证据，就用注入的 `CASE_PID`，**不要自己传 pid** |
| **`self_check` 未 arm** | 返回 **0** | 语义是「没动过手 = 没有属于本框架的状态可查」，**不是「检查通过」** |

> 为什么 `clear` 未 arm 不返回 0：`clear` 是要**动手**的动作，无事可清是个异常调用；
> 返回 2 能让「忘了 `load_env`」当场暴露，而不是静默成功。
> `self_check` 是**只读**动作，未 arm 时确实无状态可查，返回 0 是准确的。

### 三条硬约束

1. **`CASE_OUT` 必须指向最终成品**。
   跨 `run_case` 需要保留的状态（`.part` / `.part.state`）**不得**放在这个路径上 ——
   第二次 `run_case` 起跑的 `rm` 会把它删掉，**T3.3 自毁**。
2. **`lib` 不碰 `.part*`**。场景要干净起步，必须在**自己的准备段**里删；
   残留的 `.part.state` 会让 T4.1 的 `part_state=absent(expect=...)` 拿到**上一轮的证据**
   （这是 P0-1「假 PASS」的同一缺陷类，换了个文件）。
3. **清理残留进程用 `pkill -f "^<cmdline>"`**，不要裸子串 `-f "<cmdline>"` ——
   实测会匹配到自己的 ssh / shell 包装进程，把整条会话杀掉（exit 255、输出全丢、白跑一轮）。

### 采样期的 `ss` stderr

只保留**首条（≤3 行）**，以 `ss_err=` 进判定行。这是有意的取舍：
不限长的话 120s 能累积 17KB 全挤进一行判定，判定行就没法读了。

---

### 期望并发以**实测**为准，不能按 `--piece-conns` 名义值反推

`--piece-conns` 只是**上限**：实际连接数 = `min(piece_conns, 分片数)`，还会被源端 backlog 截断。

实测（2026-10-21，Windows 本机，`GetTcpTable` 采样 ~0.5ms/次）：

| 文件 | 分片数 | `--piece-conns` | 实测峰值连接 | 备注 |
|---|---|---|---|---|
| 16 MB | **4** | 64 | **4** | `<8MB` 不分片 · `8..256MB` 4MB/片 |
| 128 MB | 32 | 32 | **8–9** | backlog=5，耗时 **2.14s**（SYN 重传）|
| 128 MB | 32 | 32 | **27–36** | backlog=128 后，耗时 **0.10s** |
| 256 MB | 32 | 32 | **31** | backlog=128 后 |
| 1 GB | **64** | 64 | **64** | 窗口够长，backlog=5 时也能爬到 |

⇒ **按名义值推期望会高估 2–3 倍**（128MB 档名义 32 vs 修前实测 8–9）。
所有涉及并发的判据，期望值必须来自这类实测，**不能从参数反推**。

⚠️ 期望区间写 **27–36**，**不要写死 32** —— 峰值是采样量，会出现 36 > 名义值
（刚完成但 socket 尚未关闭的连接被计入）。**决定性判据是 `ListenOverflows = 0`**（累计量，不依赖采样率）。

#### ⭐ 峰值不是配置常量，是"竞赛结果"

同一配置、同一代码，不同时刻能测出 **11** 也能测出 **32**。原因是：
**「worker 启动速度 vs 单片完成速度」的竞赛** —— 机器越闲、传得越快、窗口越短，连接重叠越少；
越忙越重叠。所以**连接数峰值天然受机器负载污染**。

⇒ **凡 A/B 对照必须交替多轮**：一轮 A、一轮 B、一轮 A…… 并在报告里**写明轮次顺序**。
非交替的跨时间块比较（先跑 A 两次、隔几分钟再跑 B 两次）会把「块间负载差」
记到被测变量账上 —— **本 feature 已经踩过一次**（Windows 侧的 backlog 佐证因此被撤回）。

⇒ **优先选不依赖采样的判据**：
- 内核计数器（`ListenOverflows` 累计量）
- 耗时比值（补丁/原版，同刻交替）
- 产物比对（sha1 / 字节数）

这些不受"采没采到峰值"影响。**峰值只用作辅助观察，不能单独作判据。**

⚠️ **测量陷阱**：本机 loopback 下 128MB 的传输窗口只有 **96–175ms**、16MB 只有 **48ms**。
用 `netstat` 轮询（单次 40–60ms）去测一个比轮询还短的窗口，**必然测不到峰值**。
这类探针要用 `GetTcpTable`（~0.5ms/次），只记最大值。

---

### ⭐ 探针必须先自证准，再用它的读数

**两个独立实例，同一个坑**：

| 探针 | 怎么错的 | 后果 |
|---|---|---|
| `ss` 轮询 | 单次 20–40ms，比 16MB 的传输窗口（26ms）**还长** | 读出 **0**，差点报"没有连接" |
| `netstat` 轮询 | 单次 40–60ms | 同上 |
| 跨负载非交替 A/B | 中间几分钟机器负载变了 | 把负载差记到被测变量账上，得出**反的结论** |

⇒ **纪律**：任何探针/对照**先证它数得准，再用它的读数**。
- 采样类：手工撑起 0/1/4/8/16/32 已知量逐档核对
- 对照类：**交替多轮**（A/B/A/B/A/B），写明轮次顺序
- 能用累计量（内核计数器）就别用采样量

### ⚠️ 每次起源前先杀旧源（实测踩过）

同一端口只能有一个源。忘了杀 → `bind` 失败 → **第二个"对照"其实下的是旧数据**。
实测：起第二个假源时旧源还在，"16MB 对照"又下了一次 8MB，峰值 3 被误读成"16MB 只有 2 片"，
**差点把错误结论写进报告**。

```bash
pkill -f "^python3 .*fakeorigin.py"; sleep 0.5   # 用 ^ 锚定，别裸子串
```

### ⚠️ 失败前约 **28–40s** 都在重试（写死这个数）

`download.rs:175` 与 `:394` 是**两层重试**，`retries=3`，退避 `1<<attempt`（1+2+4s）
⇒ 单分片约 7s，整包 3 轮 ⇒ **总预算 28–40s**。
实测：RST 持续 **40s** 仍然 `recovered=yes`（44.9s 完成）。

⇒ 任何「期望超时 / 期望失败」的场景，`TIMEOUT_S` 与判据都要按这个数留余量，
否则你测到的是"**还在重试中**"，不是"失败了"。

---

## 已知不在本 feature 内（防范围蔓延）

- 信任链两个校验缺口（spec §7 留痕，`DESIGN §8.2` 有账）
- L1 假源「错/缺」场景
- Windows VM 的 UAC / 文件占用 / exFAT

---

## rev2 修订记录

| finding | 处理 |
|---|---|
| F-02 3 次 vs 2 次 | T6.1 改为 `--repeat 3`，验收总表同步 |
| F-06 单位差 8 倍 | 全篇 bps / bit；T5.1 序列改 `80mbit→8mbit→80mbit` |
| F-12 128MB 缺失 | 批次 4 统一 `SIZE_MB: 128` |
| G-01 三接口无任务 | T0.2 新增（`start_origin` / `apply` / `run_case` / `assert_local_only`）|
| G-02 批次 4/5 缺输入输出验证 | T4.1–4.3、T5.1–5.2 全部补齐；6 个场景文件名逐个点名 |
| G-04 阶跃无法机械判定 | T5.1 产出 `aimd_trace.csv`；T5.2 给 awk 断言 |
| G-05 T3.2/T3.3 缺输入 | 已补 |
| G-06 验收表标错源 | 验收总表加「来源」列，区分 spec / design |
| G-07 验收表漏 §4.1/§4.3/§4.5 | 已补三行 |
| 新增 | T0.4 `expected_sha1.sh`（D8）、T2.2 sudo 阻塞、T2.4 从「能力探测」升为独立任务 |
