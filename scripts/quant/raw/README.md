# scripts/quant/raw —— 只放「结论需要的」产物

规则：**可重跑的原始 trace / 逐轮日志不入库**。理由：`strace -ff` 一次 96 MiB 下载就产 37 个
per-thread 文件、18 轮就是 967 个；它们对复核没有增量信息，因为判据已经从里面提出来了
（见 `e3-align-*.txt`），重跑一次脚本比翻这些文件更快。

| 文件 | 是什么 | 复现方式 |
|---|---|---|
| `e1-kali.csv` | E1 吞吐扫（环路 + netem 400 mbit，各 6 conns × 3 轮）+ 两个地板 | `bash kali_quant.sh e1` / `e1netem` |
| `e1-floorpar.csv` | 并发地板（c 个 curl → /dev/null，不落盘） | `bash floorpar.sh` |
| `e1-hops.csv` → `e1-hops-parsed.csv` | H 的来源记录 → `hops.py` 解析结果 | `bash kali_quant.sh e1hops` + `python hops.py` |
| `fit-input.csv` | 双路径共用输入（T 与 H 配对，H 取同 conns 中位） | `python build_inputs.py` |
| `piece-times.csv` | 每次分片响应的服务时间（P50/P95/P99 的原料） | `python build_inputs.py`（读 rangeserv 日志） |
| `e5-windows.csv` / `e5-waves.csv` | 恒 500 的 retry0/retry3 结果 / 重试波次（间隔与同刻极差） | `win_e5` 见报告 §11；`build_inputs.py` 切波 |
| `e2-windows.csv` | E2 写放大（D: 1/4/16 + C: 16 对照） | `win_round.ps1` |
| `e6-windows.csv` | E6 预分配臂（D: 与 C:） | `win_round.ps1 -Prealloc` |
| `e3-align.csv` | E3 三臂（legacy / before-mid / after）墙钟与片参数 | `bash kali_quant.sh e3` |
| `e7-fallback.csv` | E7 五臂（A-c4 / B-c4 / A-c1 / B-c1 / C-stream）交错三轮：请求数、被拒请求数、额外下发字节、墙钟、SHA1 | `bash kali_e7.sh` |
| `e7-B-c4-r1.log` / `e7-B-c4-r1.out` | 代表性一轮：源端逐请求记录（含 `ev=broken` 的被 abort 响应）+ 客户端出现回退行的那次输出 | 同上 |
| `e3-align-before.txt` / `e3-align-after.txt` | **判据本身**：每次数据写的 `lseek` 偏移与长度、`off % 4096` | `python hops.py <strace> --offsets …` |

原始 per-thread strace（`e1f-*` / `e3f-*`）、逐轮 `out-*.log` / `err-*.log`、假源访问日志
（`server-win.log`、`origin-*.log`）**已删除**，需要时按报告 §11 的命令重跑。

## #8 的量尺：写跳变次数 `H`

```bash
# 1) 量尺自检（合成 strace，H 已知；8 例全过才可信）
python scripts/quant/hops_write.py --selftest

# 2) 抓 trace 并出 CSV（在 Kali 上；1 GiB × --piece-conns 1/4/16 × 3 轮，落 ext4）
bash scripts/quant/kali_hops.sh                      # 默认用 target-e7 的二进制
bash scripts/quant/kali_hops.sh ~/quant-build/xxx/release/mc-downloader   # after 用同一条

# 3) 单独解析某一份 trace（after 时手边已有 trace 也能直接量）
python scripts/quant/hops_write.py <log 或 -ff 前缀> --path-substr q1g.bin \
    --conns 16 --piece-size 16777216 --pieces 64 --csv scripts/quant/raw/hops-write.csv

# 4) 独立核对「尺子读到的写序」是不是解析假象（源端完成序 vs strace 写序）
python scripts/quant/crosscheck_order.py <order.csv> <hops-origin-c16-r1.log>
```

口径（继承 `scripts/quant/hops.py`，没有另发明）：按时刻排序同一文件上的写事件，
`w[i].off != w[i-1].off + w[i-1].len` 记一次跳变，`H` = 跳变次数；同时给
`hops_back`（回退）与 `hops_forward_gap`（前跳空洞）两个拆分。

| 文件 | 是什么 |
|---|---|
| `hops-write.csv` | 三档 conns × 3 轮：`H`、片数、片大小、写次数、下发字节、`H/(片数-1)` |
| `order-c{1,4,16}-r1.csv` | `hops_write.py --order-out` 导出的写序（每行 `seq,off,len`），交叉核对的输入 |
| `hops-origin-c16-r1.log` | 代表性一轮源端日志：给 `crosscheck_order.py` 提供独立的完成序 |

### before 数据（Kali/ext4，1 GiB，16 MiB 片，64 片）

| conns | H（三轮） | 中位 | H/(片数-1) | 判据 `conns=1⇒0`、`conns≥4⇒≈63` |
|---|---|---|---|---|
| 1 | 0 / 0 / 0 | **0** | 0.00 | 通过 |
| 4 | 24 / 15 / 25 | **24** | 0.38 | **不成立**（见下） |
| 16 | 50 / 53 / 49 | **50** | 0.79 | **不成立**（见下） |

「不成立」不是尺子的问题，三条独立证据：
① 自检 8/8（含 pwrite64 混用、两 fd 同文件、部分写、unfinished/resumed）；
② 与源端完成序交叉核对：`conns=1` **64/64 完全一致、位移 0**，`conns=16` 有 20–26/64 同位、
平均位移 1.19（写盘发生在收完之后，少量本地交换是预期的）；
③ 每轮 `writes = 64 = pieces`、每笔恰 16 MiB、合计 1 GiB ⇒ 没有漏读或多读。
原因是**调度器按升序发片、worker 成波完成**，相邻写经常正好接在上一次的尾部，
所以 H 低于"随机排列"的 62。⇒ `#8` 的验收基线要按实测钉：**before = 50（c16）/ 24（c4）**。

**平台限制**：本尺子靠 `strace`，只能在 Linux 上量；`#8` 验收里写的「D: 上」目前**无法用同一把尺子量**，
要么接受 Linux 代理口径（同一份代码、不同 OS/文件系统），要么在应用侧记写偏移（跨平台、可进 CI）。
