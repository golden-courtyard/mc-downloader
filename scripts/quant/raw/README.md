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
