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
    **`fetch` 路径并发度恒为 1**（只造 1 个 job，`buffer_unordered` 作用在 1 个任务上），用它判据会空转
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

### T2.3 VM 内可编译

- **现状**：Rust **未安装**（`cargo not found`）
- **输出**：优先 VM 内装 rustup（装到 `~/.cargo`，**不需要 sudo**）；失败退本机交叉编译 `x86_64-unknown-linux-musl`
- **验证**：VM 内 `./mc-downloader --version` → `0.5.0`；且版本 ≥ `rust-version = 1.99`

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

### T3.1 `rate` 四档

- **输入**：`lib.sh`（`apply` / `run_case` / `assert_rate`）+ `fakeorigin.py` + `expected_sha1.txt`
- **输出**：4 个场景
  `scenarios/l2_rate_800kbit.sh` · `l2_rate_8mbit.sh` · `l2_rate_80mbit.sh` · `l2_rate_400mbit.sh`
- **判据**：落盘 sha1 一致 **且** 实测吞吐 ∈ `[0.8×, 1.2×] × 目标 bps`
- **验证**：`bash scenarios/l2_rate_8mbit.sh` → `PASS l2_rate_8mbit rate=... expect=8000000`
- **注意**：`800kbit` 档下 16MB 文件约需 160s，**接近 `timeout_s` 上限**，
  必要时该场景用 4MB 文件并在注释里写明

### T3.2 `delay` 两档

- **输入**：同 T3.1
- **输出**：`scenarios/l2_delay_20ms.sh` · `l2_delay_200ms.sh`
- **判据**：完成且 sha1 一致；总耗时下界符合 `delay × 分片数 ÷ 并发`
- **验证**：`bash scenarios/l2_delay_200ms.sh`

### T3.3 中途 RST

- **输入**：同 T3.1 + `iptables`
- **输出**：`scenarios/l2_rst_mid.sh`
- **手法**：下载到约 50% 时从另一进程注入 `REJECT --reject-with tcp-reset`
- **判据（两条都要看）**：
  1. 重试预算内恢复并完成 → `PASS`
  2. 若报错退出：exit≠0 **且** `.part.state` 记录的分片数 > 0 **且** 下次重跑能续上
- **验证**：`bash scenarios/l2_rst_mid.sh`
- **要点**：这条测的是**进程活着时的 Err 分支**，与之前「强杀进程」测的不是同一条路径

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

### T4.1 `loss` 三档

- **输入**：`lib.sh` + `fakeorigin.py`（`SIZE_MB=128`）+ `expected_sha1.txt` 的 128MB 行
- **参数**：显式 `--piece-conns 32`（否则只有 4 条连接）
- **输出**：`scenarios/l2p_loss_1.sh` · `l2p_loss_5.sh` · `l2p_loss_20.sh`
- **判据**：1%/5% 下 sha1 一致；20% 下若超时，**`.part.state` 不得留下"可续传"的假象**
- **验证**：`bash scenarios/l2p_loss_5.sh`

### T4.2 `reorder` 两档

- **输入**：同 T4.1
- **参数**：显式 `--piece-conns 32`
- **输出**：`scenarios/l2p_reorder_10.sh` · `l2p_reorder_25.sh`
- **判据**：sha1 一致。**不一致就说明我们的偏移算法在并发写时错了**
- **验证**：`bash scenarios/l2p_reorder_25.sh`

### T4.3 `duplicate`

- **输入**：同 T4.1
- **参数**：显式 `--piece-conns 32`
- **输出**：`scenarios/l2p_dup_10.sh`
- **判据**：sha1 一致（TCP 应去重；不一致说明我们自己的 `expect` 长度校验有问题）
- **验证**：`bash scenarios/l2p_dup_10.sh`

---

## 批次 5 — AIMD 验证（1 个场景）

### T5.1 阶跃脚本

- **输入**：`lib.sh` + `MC_TUNE_LOG`（T0.7）+ `fakeorigin.py`
- **输出**：`scenarios/aimd_step.sh`，序列 **`80mbit → 8mbit → 80mbit`**（bit 制，与 §5.1.1 同源），每段 25s
- **产物（审计 G-04 要求「可机械判定」）**：`aimd_trace.csv`，列 `t_sec,bytes_delta,tput_bps,avail_permits`
  —— 由脚本把进度行与 `[tune]` 行按时间戳对齐后写出，**不靠人读图**
- **验证**：`bash scenarios/aimd_step.sh && head -3 /tmp/aimd_trace.csv`

### T5.2 跑并归因

- **输入**：T5.1 的 `aimd_trace.csv`
- **输出**：判定行 + 归因结论（写进 `report.md`）
- **判据**：
  | 段 | 期望 |
  |---|---|
  | 降速段（t ∈ [25,50)）| 吞吐收敛到 `8mbit` 的 ±20% |
  | 升速段（t ∈ [50,75)）| 吞吐**上升**，且 `avail_permits` 单调不减 |
  | 对照 `--adaptive` 关闭 | 吞吐不高于 `piece_conns × 8mbit` |
- **验证**：`cat /tmp/aimd_trace.csv | awk -F, '...' ` 断言上面三条
- **失败判定**：若「关掉 adaptive 也收敛」→ 说明是 TCP 在自适应、**tuner 没起作用**，记为缺陷

---

## 批次 6 — 汇总与文档回填

### T6.1 `run_all.sh` 与报告

- **输入**：14 个场景脚本 + design §2.2 的 `emit_report`
- **输出**：`run_all.sh --repeat N` 与 `report.md`
- **验证（三项都要）**：
  ```bash
  bash run_all.sh --repeat 3            # 三次结论一致，否则 FAIL
  grep -c "^| " report.md               # 行数 = 14 + 表头
  ssh kali@VM "tc qdisc show dev lo"    # 无 netem 残留
  ```
- **耗时断言**：逐场景实测耗时 ≤ 元数据 `TIMEOUT_S`，且 `TIMEOUT_S` ≤ 300
- **`ASSERT` 拦截**：任一场景缺 `# ASSERT:` 元数据 → 直接 `FAIL(missing_assert)`，不执行

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
| 128 MB | 32 | 32 | **11–15** | 原版假源，`request_queue_size` 默认 5 |
| 128 MB | 32 | 32 | **28–32** | `request_queue_size=128` 后 |
| 256 MB | 32 | 32 | 18 | 同 backlog=5 的限制 |
| 1 GB | **64** | 64 | **64** | 窗口 1044ms，够爬 |

⇒ **按名义值推期望会高估 2–3 倍**（128MB 档名义 32 vs 实测 11–15）。
所有涉及并发的判据，期望值必须来自这类实测，**不能从参数反推**。

⚠️ **测量陷阱**：本机 loopback 下 128MB 的传输窗口只有 **96–175ms**、16MB 只有 **48ms**。
用 `netstat` 轮询（单次 40–60ms）去测一个比轮询还短的窗口，**必然测不到峰值**。
这类探针要用 `GetTcpTable`（~0.5ms/次），只记最大值。

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
