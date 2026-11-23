#!/usr/bin/env python3
"""e1-throughput 的「标准化」层：raw → tidy CSV。

**这里只做标记与换算，不做统计**（统计在 calc.R）。分出这一层的原因是：
口径一旦混进数据，换个人就不知道 `mbps=481.36` 是 MB/s 还是 MiB/s；
分开之后，`calc.R` 里的每一个判据才有东西可指。

用法：python scripts/quant/e1-throughput/standardize.py
产物：scripts/quant/e1-throughput/out/tidy.csv
判据：raw 的 `mbps` 必须等于 `bytes/1048576/secs`（容差 1e-6）—— 不等就说明列名与语义对不上，
      这是「让错误可见」的第一道检查。
"""

import csv
import json
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parents[2]           # 仓库根
RAW = ROOT / "scripts" / "quant" / "raw" / "e1-kali.csv"
OUT = HERE / "out" / "tidy.csv"


def main() -> int:
    cfg = json.loads((HERE / "config.json").read_text(encoding="utf-8"))
    dropped = set(cfg["dropped_labels"])
    rows = list(csv.DictReader(RAW.open(encoding="utf-8")))
    if not rows:
        print("raw 为空", file=sys.stderr)
        return 1

    OUT.parent.mkdir(parents=True, exist_ok=True)
    unit_errors = 0
    kept = 0
    with OUT.open("w", newline="", encoding="utf-8") as f:
        w = csv.writer(f)
        w.writerow(["group", "conns", "run", "secs", "mib_per_s", "exit_ok", "unit_check"])
        for r in rows:
            if r["label"] in dropped:
                continue
            b = float(r["bytes"])
            s = float(r["secs"])
            declared = float(r["mbps"])
            derived = b / 1048576.0 / s
            # 容差不是为了掩盖差异，而是**把两个四舍五入传播出来**：
            #   mbps 保留两位小数        ⇒ ±0.005
            #   secs 保留三位小数        ⇒ 换算到吞吐是 ±(bytes/MiB) * 0.0005 / s^2
            # 真正的量纲错（差 1.048576 倍或 8 倍）仍然会被抓。
            tol = 0.005 + (b / 1048576.0) * 0.0005 / (s * s) + 1e-9 * derived
            ok = abs(declared - derived) <= tol
            if not ok:
                unit_errors += 1
            w.writerow([
                r["label"], int(r["conns"]), r["tag"], s,
                f"{derived:.6f}", int(r["exit"] == "0"), "ok" if ok else "MISMATCH",
            ])
            kept += 1

    print(f"tidy: {OUT}  行数={kept}  单位不一致行={unit_errors}")
    if unit_errors:
        print("注意：mbps 列与 bytes/1048576/secs 不一致 —— 先查单位，再谈结论", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
