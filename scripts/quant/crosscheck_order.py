#!/usr/bin/env python3
# ============================================================================
# scripts/quant/crosscheck_order.py —— 「尺子准不准」的独立核对
#
# 问题：hops_write.py 从 strace 里读出的写序，凭什么可信？
# 办法：用**另一个来源**给出同一批分片的完成顺序，两边对账。
#   来源 A（被测）：strace 里 write 的时刻顺序 → 片序号序列（hops_write.py --order-out）
#   来源 B（独立）：rangeserv 日志里每个 206 响应的 `时刻 + secs`＝该片被服务端写完的时刻
# 若两者大部分位置相同，说明尺子读到的顺序不是解析假象；
# 差异本身也有信息：写盘发生在收完之后，所以写序 ≈ 完成序但允许少量交换。
#
# 用法：
#   python crosscheck_order.py <order.csv> <origin.log> [--piece-size 16777216]
#   python crosscheck_order.py <order.csv> <origin.log> --offset-delta    # 看最大滞后
# 输出：一行 JSON（matches / n / max_displacement / kendall_like / first_mismatch）
# ============================================================================

import json
import re
import sys

RE_REQ = re.compile(r"^(?P<ts>\d+\.\d+)\s+ev=req tid=(?P<tid>\d+) .*start=(?P<start>\d+)")
RE_DONE = re.compile(r"^(?P<ts>\d+\.\d+)\s+ev=done tid=(?P<tid>\d+) .*sent=(?P<sent>\d+) .*secs=(?P<secs>\d+\.\d+)")


def server_order(log_path):
    """按「响应写完的时刻」排序的片起点列表（只留 >=1 MiB 的响应，即分片）。

    配对必须按 **tid**（一条连接同时只有一个在途请求）：按请求先后 FIFO 配对会错位
    —— HEAD 的 done（sent=0）若不参与配对，就会把后一个 GET 的完成时刻配到 HEAD 的
    start=0 上，整列顺序平移一位（这个 bug 就是这么被抓出来的）。
    """
    pending = {}         # tid -> start
    items = []           # (done_ts, start)
    with open(log_path, encoding="utf-8", errors="replace") as f:
        for line in f:
            m = RE_REQ.search(line)
            if m:
                pending[m.group("tid")] = int(m.group("start"))
                continue
            m = RE_DONE.search(line)
            if not m:
                continue
            tid = m.group("tid")
            start = pending.pop(tid, None)
            if start is None:
                continue
            if int(m.group("sent")) >= 1 << 20:
                items.append((float(m.group("ts")), start))
    items.sort()
    return [s for (_t, s) in items]


def strace_order(order_csv):
    out = []
    with open(order_csv, encoding="utf-8") as f:
        next(f, None)
        for line in f:
            parts = line.strip().split(",")
            if len(parts) >= 3 and int(parts[2]) >= 1 << 20:
                out.append(int(parts[1]))
    return out


def main():
    order_csv, log_path = sys.argv[1], sys.argv[2]
    a = strace_order(order_csv)
    b = server_order(log_path)
    n = min(len(a), len(b))
    matches = sum(1 for i in range(n) if a[i] == b[i])
    # 位移：a 里第 i 个元素在 b 里的位置与 i 的差
    pos = {v: i for i, v in enumerate(b)}
    disp = [abs(pos.get(v, -1) - i) for i, v in enumerate(a) if v in pos]
    res = {
        "strace_writes": len(a),
        "server_pieces": len(b),
        "compared": n,
        "position_matches": matches,
        "match_ratio": round(matches / n, 4) if n else None,
        "max_displacement": max(disp) if disp else None,
        "mean_displacement": round(sum(disp) / len(disp), 3) if disp else None,
    }
    print(json.dumps(res, ensure_ascii=False))


if __name__ == "__main__":
    main()
