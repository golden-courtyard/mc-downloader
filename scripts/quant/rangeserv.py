#!/usr/bin/env python3
# ============================================================================
# scripts/quant/rangeserv.py —— 定量实验用的「静态文件 + Range」源
#
# 与 scripts/net_faults/fakeorigin.py 的分工：
#   fakeorigin.py  按偏移**现场生成**内容（net_faults 的 14 个场景锁定它的 CLI，不动它）
#   rangeserv.py   serve 一个**已存在的静态文件**，只做 pread + write
# 为什么定量实验要静态文件：E2/E3/E6 是磁盘侧测量，源端 CPU 必须便宜且恒定，
#   否则「源端生成内容」的抖动会混进目标盘的写入曲线里。
#
# CLI：python3 rangeserv.py --file PATH --port P [--log PATH] [--chunk N]
#   --file  要 serve 的文件（任意大小）
#   --port  监听端口（默认 8899，与 net_faults 一致）
#   --log   请求日志（每行一条：事件/时刻/Range/线程/字节/耗时），供并发与重试时刻分析
#   --chunk 每次 socket 写的字节数（默认 262144）
#
# 支持：GET / HEAD，单区间 Range（`bytes=a-b` / `bytes=a-` / `bytes=-n`）。
#   多区间故意不支持（本项目只会发单区间）；不带 Range 时返回 200 全文件。
# 只用 python3 标准库。
# ============================================================================

import argparse
import os
import re
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOG_LOCK = threading.Lock()
LOG_FILE = None
SRC_PATH = ""
SRC_SIZE = 0
CHUNK = 262144

RANGE_RE = re.compile(r"^bytes=(\d*)-(\d*)$")


def log(fields):
    """一条日志：空格分隔的 k=v，第一列固定为 unix 时刻（浮点）。"""
    if LOG_FILE is None:
        return
    line = f"{time.time():.6f} " + " ".join(f"{k}={v}" for k, v in fields)
    with LOG_LOCK:
        LOG_FILE.write(line + "\n")
        LOG_FILE.flush()


def parse_range(header, size):
    """返回 (start, end_inclusive) 或 None（None = 整个文件）。非法 → raise ValueError。"""
    if not header:
        return None
    m = RANGE_RE.match(header.strip())
    if not m:
        raise ValueError("multi-range-unsupported")
    a, b = m.group(1), m.group(2)
    if a == "" and b == "":
        raise ValueError("empty-range")
    if a == "":
        n = int(b)
        if n == 0:
            raise ValueError("zero-suffix")
        start = max(0, size - n)
        end = size - 1
    else:
        start = int(a)
        end = size - 1 if b == "" else int(b)
    if start >= size or end < start:
        raise ValueError("unsatisfiable")
    return (start, min(end, size - 1))


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        pass  # 默认访问日志关掉，统一走 log()

    def _send_range(self, with_body):
        tid = threading.get_ident() % 1000
        t0 = time.time()
        try:
            rng = parse_range(self.headers.get("Range"), SRC_SIZE)
        except ValueError as e:
            self.send_response(416)
            self.send_header("Content-Range", f"bytes */{SRC_SIZE}")
            self.send_header("Content-Length", "0")
            self.end_headers()
            log([("ev", "416"), ("tid", tid), ("range", self.headers.get("Range") or "-"),
                 ("why", e)])
            return

        if rng is None:
            start, end = 0, SRC_SIZE - 1
            self.send_response(200)
        else:
            start, end = rng
            self.send_response(206)
            self.send_header("Content-Range", f"bytes {start}-{end}/{SRC_SIZE}")
        length = end - start + 1
        self.send_header("Content-Length", str(length))
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Accept-Ranges", "bytes")
        self.end_headers()

        log([("ev", "req"), ("tid", tid), ("start", start), ("end", end),
             ("len", length), ("method", self.command)])

        if not with_body:
            log([("ev", "done"), ("tid", tid), ("start", start), ("len", 0),
                 ("secs", f"{time.time() - t0:.6f}")])
            return

        sent = 0
        try:
            with open(SRC_PATH, "rb", buffering=0) as f:
                f.seek(start)
                left = length
                while left > 0:
                    n = CHUNK if left >= CHUNK else left
                    buf = f.read(n)
                    if not buf:
                        break
                    self.wfile.write(buf)
                    sent += len(buf)
                    left -= len(buf)
        except (BrokenPipeError, ConnectionResetError):
            log([("ev", "broken"), ("tid", tid), ("start", start), ("sent", sent)])
            return
        log([("ev", "done"), ("tid", tid), ("start", start), ("sent", sent),
             ("secs", f"{time.time() - t0:.6f}")])

    def do_GET(self):
        self._send_range(True)

    def do_HEAD(self):
        self._send_range(False)


def main():
    global LOG_FILE, SRC_PATH, SRC_SIZE, CHUNK
    ap = argparse.ArgumentParser()
    ap.add_argument("--file", required=True)
    ap.add_argument("--port", type=int, default=8899)
    ap.add_argument("--log", default="")
    ap.add_argument("--chunk", type=int, default=262144)
    a = ap.parse_args()

    SRC_PATH = os.path.abspath(a.file)
    SRC_SIZE = os.path.getsize(SRC_PATH)
    CHUNK = a.chunk
    if a.log:
        LOG_FILE = open(a.log, "a", buffering=1)

    ThreadingHTTPServer.allow_reuse_address = True           # TIME_WAIT 会让 bind 直接失败
    ThreadingHTTPServer.daemon_threads = True
    srv = ThreadingHTTPServer(("127.0.0.1", a.port), Handler)
    sys.stdout.write(f"READY size={SRC_SIZE} file={SRC_PATH} port={a.port}\n")
    sys.stdout.flush()
    srv.serve_forever()


if __name__ == "__main__":
    main()
