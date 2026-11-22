#!/usr/bin/env python3
# ============================================================================
# scripts/quant/summarize.py —— 从 raw/ 的 CSV 生成报告里的表格（数字不手抄）
#
# 报告里的每个数都必须能指回一次工具运行；手抄一遍 CSV 就是给"数字漂移"开口子。
# 本脚本只做聚合（中位/范围），不改判据。
#
# 用法：python scripts/quant/summarize.py [--md]
# ============================================================================

import csv
import os
import statistics as st
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
RAW = os.path.join(HERE, "raw")


def rows(name):
    p = os.path.join(RAW, name)
    if not os.path.exists(p):
        return []
    with open(p, newline="", encoding="utf-8") as f:
        return list(csv.DictReader(f))


def med(xs):
    xs = [x for x in xs if x is not None]
    return st.median(xs) if xs else float("nan")


def rng(xs):
    xs = [x for x in xs if x is not None]
    return (min(xs), max(xs)) if xs else (float("nan"), float("nan"))


def f2(x):
    return f"{x:.2f}"


def e2_table():
    rs = rows("e2-windows.csv")
    out = []
    for key, label in ((1, "D: HDD, conns=1"), (4, "D: HDD, conns=4"), (16, "D: HDD, conns=16")):
        g = [r for r in rs if r["label"] == "e2" and int(r["piece_conns"]) == key]
        if not g:
            continue
        out.append((label, len(g),
                    f2(med([float(r["wall_s"]) for r in g])),
                    f2(med([float(r["mbps_app"]) for r in g])),
                    f"{med([float(r['phys_wa_raw']) for r in g]):.3f}",
                    f"{med([float(r['phys_wa_net']) for r in g]):.3f}"))
    g = [r for r in rs if r["label"] == "ctrl-c-nvme"]
    if g:
        out.append(("C: NVMe, conns=16", len(g),
                    f2(med([float(r["wall_s"]) for r in g])),
                    f2(med([float(r["mbps_app"]) for r in g])),
                    f"{med([float(r['phys_wa_raw']) for r in g]):.3f}",
                    f"{med([float(r['phys_wa_net']) for r in g]):.3f}"))
    return out


def e6_table():
    rs = rows("e6-windows.csv") + rows("e2-windows.csv")
    out = []
    for label, arm in (("e6-prealloc", "D: 预分配"), ("ctrl-c-prealloc", "C: 预分配")):
        g = [r for r in rs if r["label"] == label]
        if not g:
            continue
        wall = med([float(r["wall_s"]) for r in g])
        pre = med([float(r["prealloc_s"]) for r in g])
        out.append((arm, len(g), f2(wall), f2(pre), f2(wall + pre),
                    f"{med([float(r['phys_wa_raw']) for r in g]):.2f}",
                    f2(med([float(r["mbps_app"]) for r in g])),
                    int(med([float(r["extents"]) for r in g]))))
    return out


def e1_table():
    rs = rows("fit-input.csv")
    out = []
    for group in ("e1-unlimited", "e1-netem400"):
        for c in (1, 4, 8, 16, 32, 64):
            g = [r for r in rs if r["group"] == group and int(r["conns"]) == c]
            if not g:
                continue
            out.append((group.replace("e1-", ""), c, len(g),
                        f2(med([float(r["mbps"]) for r in g])), int(g[0]["hops_H"])))
    return out


def main():
    md = "--md" in sys.argv
    if md:
        print("### E2")
        print("| arm | n | wall_s | MB/s | WA(raw) | WA(net) |")
        print("|---|---|---|---|---|---|")
        for r in e2_table():
            print("| " + " | ".join(str(x) for x in r) + " |")
        print("\n### E6")
        print("| arm | n | wall_s | prealloc_s | end-to-end | WA(raw) | MB/s | extents |")
        print("|---|---|---|---|---|---|---|---|")
        for r in e6_table():
            print("| " + " | ".join(str(x) for x in r) + " |")
        print("\n### E1")
        print("| link | conns | n | MB/s | H |")
        print("|---|---|---|---|---|")
        for r in e1_table():
            print("| " + " | ".join(str(x) for x in r) + " |")
        e5 = rows("e5-windows.csv")
        if e5:
            print("\n### E5")
            print("| arm | conns | retry | requests | exit |")
            print("|---|---|---|---|---|")
            for r in e5:
                print(f"| {r['arm']} | {r['conns']} | {r['retry']} | {r['reqs']} | {r['exit']} |")
        e3 = rows("e3-align.csv")
        if e3:
            print("\n### E3")
            print("| arm | n | 片大小 | 墙钟(s) | MB/s |")
            print("|---|---|---|---|---|")
            for arm in ("legacy", "before-mid", "after"):
                g = [r for r in e3 if r["arm"] == arm]
                if not g:
                    continue
                ps = g[0]["piece_line"].split("片大小 ")[-1].split(" 字节")[0] if "片大小" in g[0]["piece_line"] else "-"
                print(f"| {arm} | {len(g)} | {ps} | "
                      f"{f2(med([float(r['secs']) for r in g]))} | "
                      f"{f2(med([float(r['mbps']) for r in g]))} |")
    else:
        for t, name in ((e2_table(), "E2"), (e6_table(), "E6"), (e1_table(), "E1")):
            print(f"== {name} ==")
            for r in t:
                print("  ", r)


if __name__ == "__main__":
    main()
