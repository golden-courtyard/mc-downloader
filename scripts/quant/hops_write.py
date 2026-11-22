#!/usr/bin/env python3
# ============================================================================
# scripts/quant/hops_write.py —— #8 的量尺：数「写跳变次数 H」
#
# 口径**直接从 scripts/quant/hops.py 继承，不另发明**：
#   按时刻排序某个文件上的写事件 w1..wn，凡 `w[i].off != w[i-1].off + w[i-1].len`
#   记一次跳变；H = 跳变次数。严格顺序写 ⇒ H = 0；n 片乱序完成 ⇒ H ≈ n-1。
#
# 与 hops.py 的差别（为什么不能直接复用那个脚本）：
#   hops.py 是 E3 用的，只认「单文件 + lseek+write + 单一线程」，靠 `-ff` 每线程一个文件。
#   #8 的验收要量的是「同一个文件在**多个 fd/多个线程**上的写序」——
#   分片下载给每片**各自**打开一个句柄，所以按 fd 分组会得到 n 条各自单调的序列、
#   H 恒为 0（这就是"尺子不准"的典型形态）。必须按**文件**分组，
#   而"哪个 fd 是哪个文件"只能从 strace 的 `-y` 装饰里读（`write(17</path/x.part>, ...)`）。
#
# 支持的三种 syscall：
#   lseek(fd, OFF, SEEK_SET) = OFF   记下该 (tid, fd) 的绝对偏移
#   write(fd, buf, LEN) = RET        从记下的偏移起写 RET 字节（RET 可能 < LEN）
#   pwrite64(fd, buf, LEN, OFF) = RET 偏移自带，不改 fd 偏移
# 输入既可以是单个 `-o log` 文件，也可以是 `-ff -o prefix` 的一堆 prefix.<tid>；
# 单文件 `-f` 输出里被拆成 `<unfinished ...>` / `<... resumed>` 的行会被合并回来。
#
# 用法：
#   python hops_write.py <log 或 前缀> [--path-substr q1g.bin] [--run label]
#                        [--conns 16] [--piece-size 16777216] [--pieces 64]
#                        [--csv raw/hops-write.csv]
#   python hops_write.py --selftest        # 量尺自检（合成 strace，H 已知）
#
# 判据（#8，先写死）：conns=1 ⇒ H == 0；conns≥4 ⇒ H ≈ pieces-1。
# ============================================================================

import csv
import glob
import os
import re
import sys

# ---------------------------------------------------------------- 解析
FD = r"(?P<fd>\d+(?:<[^>]*>)?)"
TS = r"(?:(?P<tid>\d+)\s+)?(?P<h>\d+):(?P<m>\d+):(?P<s>[\d.]+)\s+"

RE_SEEK = re.compile(TS + r"lseek\(" + FD + r",\s*(?P<off>-?\d+),\s*SEEK_SET\s*\)\s*=\s*(?P<ret>-?\d+)")
RE_WRITE = re.compile(TS + r"write\(" + FD + r",\s*(?:\"(?:[^\"\\]|\\.)*\"|0x[0-9a-fA-F]+|NULL)"
                      r"(?:\.\.\.)?,\s*(?P<len>\d+)\s*\)\s*=\s*(?P<ret>-?\d+)")
RE_PWRITE = re.compile(TS + r"pwrite64\(" + FD + r",\s*(?:\"(?:[^\"\\]|\\.)*\"|0x[0-9a-fA-F]+|NULL)"
                       r"(?:\.\.\.)?,\s*(?P<len>\d+),\s*(?P<off>\d+)\s*\)\s*=\s*(?P<ret>-?\d+)")
RE_RESUMED = re.compile(r"^(?P<tid>\d+)\s+<\.\.\.\s+\w+\s+resumed>(?P<rest>.*)$")
RE_UNFINISHED = re.compile(r"^(?P<tid>\d+)\s+(?P<ts>[\d:.]+)\s+(?P<body>.*)<unfinished \.\.\.>$")


def expand_inputs(args):
    files = []
    for a in args:
        if os.path.isfile(a):
            files.append(a)
        else:
            files.extend(sorted(glob.glob(a + ".*")))
    return files


def logical_lines(path):
    """把单文件 `-f` 输出里被拆开的 unfinished/resumed 合并成一条逻辑行。

    返回 (tid, 时间戳秒, 文本)。tid 缺失（非 -f）时记为 "0"。
    """
    pending = {}
    out = []
    with open(path, "r", errors="replace") as f:
        for raw in f:
            line = raw.rstrip("\n")
            m = RE_RESUMED.match(line)
            if m:
                tid = m.group("tid")
                body = pending.pop(tid, None)
                if body is not None:
                    out.append(body.rstrip() + " " + m.group("rest").lstrip(">"))
                continue
            m = RE_UNFINISHED.match(line)
            if m:
                pending[m.group("tid")] = m.group("tid") + " " + m.group("ts") + " " + m.group("body")
                continue
            tid = line.split(" ", 1)[0] if line[:1].isdigit() else "0"
            out.append(line)
    return out


def fd_path(fdtok):
    """`17</home/kali/x.part>` → '/home/kali/x.part'；没有 -y 装饰时返回 None。"""
    i, j = fdtok.find("<"), fdtok.rfind(">")
    if i >= 0 and j > i:
        return fdtok[i + 1:j]
    return None


def parse_events(files):
    """返回按时刻排序的写事件列表 [(t, tid, fd, path, off, length)] 与诊断信息。"""
    raws = []
    for p in files:
        for line in logical_lines(p):
            m = RE_SEEK.search(line)
            if m:
                raws.append(("seek", m))
                continue
            m = RE_PWRITE.search(line)
            if m:
                raws.append(("pwrite", m))
                continue
            m = RE_WRITE.search(line)
            if m:
                raws.append(("write", m))

    def tkey(m):
        return int(m.group("h")) * 3600 + int(m.group("m")) * 60 + float(m.group("s"))

    raws.sort(key=lambda x: tkey(x[1]))
    seek_off = {}          # (tid, fdnum) -> 绝对偏移
    fd_off = {}            # (tid, fdnum) -> 当前偏移（写完推进）
    events = []
    n_partial = 0
    for kind, m in raws:
        tid = m.group("tid") or "0"
        fdtok = m.group("fd")
        fdnum = fdtok.split("<")[0]
        ret = int(m.group("ret"))
        if kind == "seek":
            seek_off[(tid, fdnum)] = int(m.group("off"))
            fd_off[(tid, fdnum)] = int(m.group("off"))
            continue
        req = int(m.group("len"))
        if ret <= 0:
            continue
        if ret < req:
            n_partial += 1
        if kind == "pwrite":
            off = int(m.group("off"))
        else:
            off = seek_off.pop((tid, fdnum), None)
            if off is None:
                off = fd_off.get((tid, fdnum), 0)
        fd_off[(tid, fdnum)] = off + ret
        events.append((tkey(m), tid, fdnum, fd_path(fdtok), off, ret))
    return events, {"partial_writes": n_partial}


def count_hops(events, path_substr=None, min_bytes=4096):
    """按文件分组的写跳变计数。

    - 分组键：有 -y 时用路径；没有 -y 时退回 fd（**并标记 degraded**，因为多句柄同文件会失真）。
    - 过滤：只留 >= min_bytes 的写（滤掉 stdout / eventfd 这类几字节控制写）。
    """
    data = [e for e in events if e[5] >= min_bytes]
    degraded = not any(e[3] for e in data)
    if path_substr:
        sel = [e for e in data
               if (e[3] is not None and path_substr in e[3])
               or (e[3] is None and path_substr in (e[3] or ""))]
        if not sel:                      # 没有 -y 时无法按路径过滤：退回全量数据写
            sel = data
            degraded = True
    else:
        sel = data

    groups = {}
    for (t, tid, fd, path, off, ln) in sel:
        key = path if path else f"fd:{fd}"
        groups.setdefault(key, []).append((t, off, ln))

    per_file = []
    for key, seq in groups.items():
        seq.sort()
        hops = back = fwd = 0
        prev = None
        for (_t, off, ln) in seq:
            if prev is not None:
                end = prev + prev_len
                if off != end:
                    hops += 1
                    if off < end:
                        back += 1
                    else:
                        fwd += 1
            prev, prev_len = off, ln
        per_file.append({
            "file": key,
            "writes": len(seq),
            "bytes": sum(x[2] for x in seq),
            "hops": hops,
            "hops_back": back,
            "hops_forward_gap": fwd,
            "distinct_starts": len({x[1] for x in seq}),
            "first_offsets": [x[1] for x in seq][:8],
        })
    if per_file:
        per_file[0]["sequence"] = sorted(groups[per_file[0]["file"]]) if per_file and per_file[0]["file"] in groups else []
    per_file.sort(key=lambda d: -d["bytes"])
    total_hops = sum(d["hops"] for d in per_file)
    return per_file, total_hops, degraded


# ---------------------------------------------------------------- 自检
SELFTEST_CASES = []


def _mk(ts, tid, body):
    return f"{tid} 22:00:{ts:06.3f} {body}"


def _seek(ts, tid, fd, off, path=None):
    tokm = f"{fd}<{path}>" if path else str(fd)
    return _mk(ts, tid, f"lseek({tokm}, {off}, SEEK_SET) = {off}")


def _write(ts, tid, fd, ln, ret=None, path=None):
    tokm = f"{fd}<{path}>" if path else str(fd)
    ret = ln if ret is None else ret
    return _mk(ts, tid, f'write({tokm}, "\\x00\\x00"..., {ln}) = {ret}')


def _pwrite(ts, tid, fd, ln, off, path=None):
    tokm = f"{fd}<{path}>" if path else str(fd)
    return _mk(ts, tid, f'pwrite64({tokm}, "\\x00\\x00"..., {ln}, {off}) = {ln}')


def build_selftest():
    global SELFTEST_CASES
    P = "/w/q1g.bin.part"
    cases = []

    # ① 顺序写：H = 0
    lines = []
    for i in range(4):
        lines += [_seek(1 + i * 0.01, 100, 17, i * 100, P), _write(1 + i * 0.01 + 0.001, 100, 17, 100, path=P)]
    cases.append(("sequential lseek+write", lines, 0))

    # ② 打乱：H = 3（4 片随机序）
    order = [300, 0, 200, 100]
    lines = []
    for i, off in enumerate(order):
        lines += [_seek(1 + i * 0.01, 100, 17, off, P), _write(1 + i * 0.01 + 0.001, 100, 17, 100, path=P)]
    cases.append(("shuffled 4 pieces", lines, 3))

    # ③ 纯 pwrite64 顺序：H = 0
    lines = [_pwrite(1 + i * 0.01, 100, 17, 100, i * 100, P) for i in range(4)]
    cases.append(("pwrite64 sequential", lines, 0))

    # ④ pwrite64 + write 混用、打乱：H = 3
    lines = [_pwrite(1.00, 100, 17, 100, 300, P),
             _seek(1.01, 100, 18, 0, P), _write(1.011, 100, 18, 100, path=P),
             _pwrite(1.02, 100, 17, 100, 200, P),
             _seek(1.03, 100, 19, 100, P), _write(1.031, 100, 19, 100, path=P)]
    cases.append(("mixed pwrite64/write shuffled", lines, 3))

    # ⑤ 两个 fd 指向同一文件（-y）：按**文件**分组才对；按 fd 分组会得到 H=0
    lines = [_seek(1.00, 100, 17, 200, P), _write(1.001, 100, 17, 100, path=P),
             _seek(1.01, 200, 18, 0, P), _write(1.011, 200, 18, 100, path=P),
             _seek(1.02, 300, 19, 300, P), _write(1.021, 300, 19, 100, path=P),
             _seek(1.03, 400, 20, 100, P), _write(1.031, 400, 20, 100, path=P)]
    cases.append(("two fds same file (needs -y)", lines, 3))

    # ⑥ 部分写：第一次只写出 60/100，下一次从 60 继续 ⇒ 不该记跳变
    lines = [_seek(1.00, 100, 17, 0, P), _write(1.001, 100, 17, 100, ret=60, path=P),
             _write(1.002, 100, 17, 40, ret=40, path=P),
             _seek(1.01, 100, 17, 100, P), _write(1.011, 100, 17, 100, path=P)]
    cases.append(("partial write advances by ret", lines, 0))

    # ⑦ 旁路 fd（stdout 40 B、eventfd 8 B）不该被计入
    lines = [_write(1.000, 100, 1, 40, path="pipe:[1234]"),
             _write(1.001, 100, 4, 8, path="anon_inode:[eventfd]")]
    for i in range(4):
        lines += [_seek(1 + i * 0.01, 100, 17, i * 100, P), _write(1 + i * 0.01 + 0.001, 100, 17, 100, path=P)]
    cases.append(("control fds filtered out", sorted(lines), 0))

    # ⑧ 被拆开的 unfinished/resumed 行
    # 这条是**有载荷**的：若 unfinished/resumed 没被合并，thread 100 的 write 会退回偏移 0，
    # 序列变成 0..100 然后 300..400 ⇒ H=1。合并成功则 200..300/300..400 ⇒ H=0。
    lines = [f"100 22:00:01.000000 lseek(17<{P}>, 200, SEEK_SET <unfinished ...>",
             "100 <... lseek resumed>) = 200",
             f"100 22:00:01.200000 write(17<{P}>, \"\\x00\"..., 100) = 100",
             f"200 22:00:01.250000 lseek(18<{P}>, 300, SEEK_SET) = 300",
             f"200 22:00:01.400000 write(18<{P}>, \"\\x00\"..., 100) = 100"]
    cases.append(("unfinished/resumed merged", lines, 0))
    SELFTEST_CASES = cases
    return cases


def run_selftest():
    import tempfile
    cases = build_selftest()
    print("=== hops_write.py 自检（合成 strace，H 已知）===")
    ok_all = True
    for name, lines, expect in cases:
        with tempfile.NamedTemporaryFile("w", suffix=".strace", delete=False, encoding="utf-8") as f:
            f.write("\n".join(lines) + "\n")
            tmp = f.name
        try:
            events, diag = parse_events([tmp])
            per_file, total_hops, degraded = count_hops(events, min_bytes=1)
            ok = (total_hops == expect) and (not degraded)
            ok_all = ok_all and ok
            print(f"  [{'PASS' if ok else 'FAIL'}] {name}: H={total_hops} 期望 {expect} "
                  f"(files={len(per_file)} writes={sum(d['writes'] for d in per_file)} "
                  f"degraded={degraded})")
        finally:
            os.unlink(tmp)
    print("自检结果:", "全部通过" if ok_all else "有失败 —— 尺子不可用")
    return 0 if ok_all else 1


# ---------------------------------------------------------------- CLI
def main():
    if "--selftest" in sys.argv:
        sys.exit(run_selftest())

    args = sys.argv[1:]
    paths, opts = [], {"run": "", "conns": "", "piece_size": "", "pieces": "",
                       "csv": "", "path_substr": "", "order_out": ""}
    i = 0
    while i < len(args):
        a = args[i]
        if a.startswith("--"):
            key = a[2:].replace("-", "_")
            if key in opts:
                opts[key] = args[i + 1]
                i += 2
                continue
            if key == "selftest":
                i += 1
                continue
        paths.append(a)
        i += 1
    if not paths:
        print(__doc__ or "usage: hops_write.py <log|prefix> [--conns N] [--csv out.csv]")
        sys.exit(2)
    files = expand_inputs(paths)
    if not files:
        print(f"找不到输入: {paths}")
        sys.exit(2)

    events, diag = parse_events(files)
    per_file, total_hops, degraded = count_hops(events, opts["path_substr"] or None)
    top = per_file[0] if per_file else {"file": "-", "writes": 0, "bytes": 0,
                                        "distinct_starts": 0, "hops_back": 0,
                                        "hops_forward_gap": 0}

    pieces = int(opts["pieces"]) if str(opts["pieces"]).isdigit() else 0
    row = {
        "run": opts["run"] or os.path.basename(paths[0]),
        "conns": opts["conns"],
        "piece_size": opts["piece_size"],
        "pieces": pieces,
        "H": total_hops,
        "total_bytes": top["bytes"],
        "files": len(per_file),
        "writes": top["writes"],
        "distinct_starts": top["distinct_starts"],
        "hops_back": top["hops_back"],
        "hops_forward_gap": top["hops_forward_gap"],
        "target": top["file"],
        "degraded_no_y": int(degraded),
        "partial_writes": diag["partial_writes"],
        "first_offsets": " ".join(str(x) for x in top.get("first_offsets", [])),
    }
    if pieces > 1:
        row["H_over_pieces_minus_1"] = round(total_hops / (pieces - 1), 4)

    if opts["order_out"]:
        with open(opts["order_out"], "w", encoding="utf-8") as f:
            f.write("seq,off,len\n")
            for i, (_t, off, ln) in enumerate(per_file[0].get("sequence", []) if per_file else []):
                f.write(f"{i},{off},{ln}\n")

    if opts["csv"]:
        new = not os.path.exists(opts["csv"])
        with open(opts["csv"], "a", newline="", encoding="utf-8") as f:
            w = csv.DictWriter(f, fieldnames=list(row))
            if new:
                w.writeheader()
            w.writerow(row)
    print(f"run={row['run']} conns={row['conns']} pieces={pieces} H={total_hops} "
          f"writes={row['writes']} bytes={row['total_bytes']} files={len(per_file)} "
          f"degraded_no_y={degraded} partial={diag['partial_writes']}")
    if degraded:
        print("  !! 警告：这份 trace 没有 -y 装饰 ⇒ 只能按 fd 分组，多句柄同文件会被拆成多条单调序列，H 会偏低。"
              "请用 strace -y 重抓。", file=sys.stderr)


if __name__ == "__main__":
    main()
