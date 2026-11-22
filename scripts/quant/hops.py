#!/usr/bin/env python3
# ============================================================================
# scripts/quant/hops.py —— 从 strace 日志提「写顺序」：H（写跳变次数）与对齐判定
#
# 为什么要自己解析：本项目的分片写是「每片一个独立文件句柄 → lseek(绝对偏移) → 一次
# write(整片)」（src/download.rs 的 spawn_blocking 那段）。strace 里成对出现
# `lseek(fd, OFF, SEEK_SET) = OFF` + `write(fd, ..., LEN) = LEN`，两条按 fd 配对就是
# 一次真实的落盘动作（时刻 / 偏移 / 长度）。
#   ⇒ issue #4 里写的 pwrite64 在本代码里根本不存在（Windows 没有 pwrite，跨平台只能
#     seek+write）；这里按**实际**发生的 syscall 对来抽，不按臆想的来。
#
# ⚠️ 为什么必须 `-ff`（每线程一个文件）：多线程共用一个输出文件时，strace 会把被阻塞的
#    syscall 拆成 `<unfinished ...>` + `<... resumed>` 两行；用单文件 `-o` 抓 16 个线程的
#    大 write，绝大多数会落进这种拆行状态，解析结果只剩个位数事件（实测如此）。
#    `-ff -o PREFIX` 让每个 tid 单独成文件，行不再被拆；再按时刻归并即得全局写序。
#
# H 的定义（先定后测，报告里照抄）：
#   按时刻排序写事件 w1..wn，凡 `w[i].off != w[i-1].off + w[i-1].len` 记一次跳变，
#   H = 跳变次数。严格顺序写 ⇒ H = 0；n 片并发完成 ⇒ 约 n-1。
#
# 用法：
#   python hops.py <文件或前缀...> [--tag 名字] [--csv out.csv]
#     参数里不含 `*` 且不是已存在文件时，按 `<前缀>.*` 展开（配合 strace -ff）
# 输出：一行 JSON（含 aligned_all / misaligned_sample，可直接当 E3 判据）
# ============================================================================

import glob
import json
import os
import re
import sys

RE_LSEEK = re.compile(r"^\s*(?:(\d+)\s+)?(\d+):(\d+):([\d.]+)\s+lseek\((\d+),\s*(-?\d+),\s*SEEK_SET\)")
RE_WRITE = re.compile(r"^\s*(?:(\d+)\s+)?(\d+):(\d+):([\d.]+)\s+write\((\d+),\s+\"(?:[^\"\\]|\\.)*\"\.\.\.,\s*(\d+)\)\s*=\s*(\d+)")
RE_WRITE_NS = re.compile(r"^\s*(?:(\d+)\s+)?(\d+):(\d+):([\d.]+)\s+write\((\d+),\s+(?:0x[0-9a-f]+|NULL|\[\])\.\.\.,\s*(\d+)\)\s*=\s*(-?\d+)")


def expand(args):
    files = []
    for a in args:
        if os.path.isfile(a):
            files.append(a)
        else:
            files.extend(sorted(glob.glob(a + ".*")))
    return files


def parse_file(path):
    last_seek = {}
    out = []
    with open(path, "r", errors="replace") as f:
        for line in f:
            m = RE_LSEEK.match(line)
            if m:
                last_seek[int(m.group(5))] = int(m.group(6))
                continue
            m = RE_WRITE.match(line) or RE_WRITE_NS.match(line)
            if not m:
                continue
            fd, req, got = int(m.group(5)), int(m.group(6)), int(m.group(7))
            if fd in last_seek and req > 0 and got > 0:
                t = int(m.group(2)) * 3600 + int(m.group(3)) * 60 + float(m.group(4))
                out.append((t, last_seek.pop(fd), got))
    return out


def summarize(events, tag="", page=4096):
    events.sort(key=lambda e: e[0])
    hops = 0
    prev = None
    for (_t, off, ln) in events:
        if prev is not None and off != prev[0] + prev[1]:
            hops += 1
        prev = (off, ln)
    # 数据写 = 不是 eventfd 那种几十字节的控制写
    data = [e for e in events if e[2] > 512]
    starts = sorted({e[1] for e in data})
    misaligned = [o for o in starts if o % page != 0]
    return {
        "tag": tag,
        "writes": len(data),
        "bytes": sum(e[2] for e in data),
        "hops": hops,
        "max_write": max((e[2] for e in data), default=0),
        "distinct_starts": len(starts),
        "misaligned_count": len(misaligned),
        "misaligned_sample": misaligned[:6],
        "aligned_all": len(misaligned) == 0,
        "starts": starts[:20],
        "last_start_len": None,
    }


def main():
    paths, tag, csv = [], "", ""
    args = sys.argv[1:]
    i = 0
    while i < len(args):
        if args[i] == "--tag":
            tag = args[i + 1]; i += 2
        elif args[i] == "--csv":
            csv = args[i + 1]; i += 2
        else:
            paths.append(args[i]); i += 1
    files = expand(paths)
    events = []
    for p in files:
        events.extend(parse_file(p))
    s = summarize(events, tag or os.path.basename(paths[0]))
    s["files"] = len(files)
    print(json.dumps(s, ensure_ascii=False))
    if csv:
        new = not os.path.exists(csv)
        with open(csv, "a", encoding="utf-8") as f:
            if new:
                f.write("tag,writes,bytes,hops,max_write,distinct_starts,misaligned_count,aligned_all\n")
            f.write(f"{s['tag']},{s['writes']},{s['bytes']},{s['hops']},{s['max_write']},"
                    f"{s['distinct_starts']},{s['misaligned_count']},{int(s['aligned_all'])}\n")


if __name__ == "__main__":
    main()

# ---- ??????????E3 ??????????????? ----
import re as _re
def dump_offsets(paths, out_path, arm):
    files = expand(paths)
    evs = []
    for p in files:
        m = _re.search(r"\.(\d+)$", p)
        tid = m.group(1) if m else "0"
        for (t, off, ln) in parse_file(p):
            evs.append((t, tid, off, ln))
    evs.sort()
    with open(out_path, "w", encoding="utf-8") as f:
        f.write("arm,tid,lseek_off,write_len,off_mod_4096\n")
        for (_t, tid, off, ln) in evs:
            if ln > 512:
                f.write(f"{arm},{tid},{off},{ln},{off % 4096}\n")
    return len([e for e in evs if e[3] > 512])

if __name__ == "__main__" and "--offsets" in sys.argv:
    i = sys.argv.index("--offsets")
    out_path = sys.argv[i + 1]
    arm = sys.argv[i + 2] if len(sys.argv) > i + 2 else "arm"
    paths = [a for a in sys.argv[1:i] if not a.startswith("--")]
    n = dump_offsets(paths, out_path, arm)
    print(f"{out_path}: {n} ????")
