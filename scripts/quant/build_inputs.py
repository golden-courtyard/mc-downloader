#!/usr/bin/env python3
# ============================================================================
# scripts/quant/build_inputs.py —— 把原始产物收敛成两条分析路径共用的输入
#
# 为什么要有这一层：Wolfram 与 R 必须**读同一份数字**，否则"双路径相对差 < 5%"
# 只能证明两边脚本各自算得一样（同义反复），不能证明结论稳。三条输入全部由本脚本
# 从 raw/ 的原始产物生成，任何一边都不许自己另抄一份常数。
#
# 产出（都在 scripts/quant/raw/）：
#   fit-input.csv   E1 的 T/H 配对（H 取同 conns 三轮的中位；T 取不挂 strace 的干净轮）
#   piece-times.csv rangeserv 日志切出的「每一次分片响应」耗时（P50/P95/P99 的原料）
#   e5-waves.csv    E5 的重试波次（同刻重试判据）
# ============================================================================

import csv
import os
import re
import statistics as st
from collections import defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))
RAW = os.path.join(HERE, "raw")


def median_by_conns(path):
    """e1-hops-parsed.csv → {conns: median H}"""
    by = defaultdict(list)
    with open(path, newline="", encoding="utf-8") as f:
        for row in csv.DictReader(f):
            c = int(re.search(r"c(\d+)-r\d+", row["tag"]).group(1))
            by[c].append(int(row["hops"]))
    return {c: st.median(v) for c, v in by.items()}


def build_fit_input():
    hops = median_by_conns(os.path.join(RAW, "e1-hops-parsed.csv"))
    out = []
    with open(os.path.join(RAW, "e1-kali.csv"), newline="", encoding="utf-8") as f:
        for row in csv.DictReader(f):
            if not row["label"].startswith("e1-"):
                continue
            c = int(row["conns"])
            out.append({
                "group": row["label"],
                "conns": c,
                "round": row["tag"].split("r")[-1],
                "bytes": int(row["bytes"]),
                "secs": float(row["secs"]),
                "mbps": float(row["mbps"]),
                "hops_H": hops[c],
            })
    p = os.path.join(RAW, "fit-input.csv")
    with open(p, "w", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=list(out[0]))
        w.writeheader()
        w.writerows(out)
    print(f"fit-input.csv rows={len(out)}  H={hops}")


RE_DONE = re.compile(
    r"^(?P<ts>\d+\.\d+) ev=done tid=(?P<tid>\d+) start=(?P<start>\d+) "
    r"sent=(?P<sent>\d+) secs=(?P<secs>\d+\.\d+)$")


def build_piece_times():
    """rangeserv 日志 → 按「突发」切轮次；并发数由区间重叠推出来，不靠我记忆里的顺序。"""
    evs = []
    with open(os.path.join(RAW, "server-win.log"), encoding="utf-8", errors="replace") as f:
        for line in f:
            m = RE_DONE.match(line.strip())
            if not m:
                continue
            secs = float(m.group("secs"))
            sent = int(m.group("sent"))
            if sent < 1024:          # HEAD / 0 字节响应不算分片
                continue
            evs.append((float(m.group("ts")) - secs, float(m.group("ts")), secs, sent))
    evs.sort()
    bursts, cur = [], []
    for e in evs:
        if cur and e[0] - cur[-1][1] > 2.0:
            bursts.append(cur); cur = []
        cur.append(e)
    if cur:
        bursts.append(cur)

    rows = []
    for i, b in enumerate(bursts):
        # 并发 = 同一时刻在途请求数的最大值
        points = []
        for (s, t, _d, _n) in b:
            points.append((s, 1)); points.append((t, -1))
        points.sort()
        conc = peak = 0
        for (_x, d) in points:
            conc += d
            peak = max(peak, conc)
        durs = [x[2] for x in b]
        rows.append({
            "burst": i + 1,
            "pieces": len(b),
            "concurrency": peak,
            "median_secs": round(st.median(durs), 4),
            "p50_secs": round(percentile(durs, 50), 4),
            "p95_secs": round(percentile(durs, 95), 4),
            "p99_secs": round(percentile(durs, 99), 4),
            "total_mb": round(sum(x[3] for x in b) / 1048576, 1),
        })
    p = os.path.join(RAW, "piece-times.csv")
    with open(p, "w", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=list(rows[0]))
        w.writeheader()
        w.writerows(rows)
    print(f"piece-times.csv rows={len(rows)}")
    for r in rows:
        print("  burst={burst:2d} pieces={pieces:3d} conc={concurrency:3d} "
              "p50={p50_secs:7.3f} p95={p95_secs:7.3f} p99={p99_secs:7.3f} mb={total_mb}".format(**r))


def percentile(xs, q):
    xs = sorted(xs)
    if not xs:
        return float("nan")
    k = (len(xs) - 1) * q / 100.0
    lo, hi = int(k), min(int(k) + 1, len(xs) - 1)
    return xs[lo] + (xs[hi] - xs[lo]) * (k - lo)


RE_REQ = re.compile(r"^(?P<ts>\d+\.\d+) ev=req .*code=(?P<code>\d+)")


def build_e5_waves():
    ts = []
    with open(os.path.join(RAW, "e5-server-cluster.log"), encoding="utf-8") as f:
        for line in f:
            m = RE_REQ.match(line.strip())
            if m:
                ts.append((float(m.group("ts")), int(m.group("code"))))
    ts.sort()
    waves, cur = [], []
    for t, code in ts:
        if cur and t - cur[-1][0] > 0.8:
            waves.append(cur); cur = []
        cur.append((t, code))
    if cur:
        waves.append(cur)
    rows = []
    for i, w in enumerate(waves):
        times = [x[0] for x in w]
        gap = round(times[0] - waves[i - 1][0][0], 3) if i else 0.0
        rows.append({
            "wave": i,
            "requests": len(w),
            "gap_from_prev_s": gap,
            "spread_ms": round((max(times) - min(times)) * 1000, 1),
            "codes": "".join(str(c) for _t, c in w),
        })
    p = os.path.join(RAW, "e5-waves.csv")
    with open(p, "w", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=list(rows[0]))
        w.writeheader()
        w.writerows(rows)
    print(f"e5-waves.csv rows={len(rows)}")
    for r in rows:
        print("  wave={wave} reqs={requests} gap={gap_from_prev_s}s spread={spread_ms}ms codes={codes}".format(**r))


if __name__ == "__main__":
    build_fit_input()
    build_piece_times()
    build_e5_waves()
