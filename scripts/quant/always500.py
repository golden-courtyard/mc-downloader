#!/usr/bin/env python3
# ============================================================================
# scripts/quant/always500.py —— E5「重试独立性」用的恒错源
#
# 为什么放在 scripts/quant/ 而不是 scripts/net_faults/scenarios/：
#   run_all.sh 有「恰好 14 个场景，否则报错退出」的判据（防"少跑一个也全绿"），
#   往里加场景会直接破坏它。issue #4 明说新增的「持续 500」另起目录。
#
# CLI：python3 always500.py --port P [--log PATH] [--status 500] [--p-fail 1.0]
#   --status  返回的状态码（默认 500）
#   --p-fail  每个请求失败的概率（默认 1.0 = 恒错）。0<p<1 时用同一随机源独立判定，
#             用来对照「独立失败」与「相关失败」两种情形 —— 判据见 issue #4 §三问题 2。
#   --log     每行一条：unix 时刻 事件=req 编号 状态 线程 Range
#
# 判据用法：
#   恒错（p-fail=1.0）⇒ p1 = 1，重试 3 次后成功率必为 0。
#   若观察值 ≈ 1-(1-p1)(1-p2)(1-p3) = 1 而不是 ≈ p1 = 1 —— 两者数值上重合，
#   所以真正的证据在**日志里的重试时刻**：恒错下 4 个分片的重试是否落在同一秒。
#
# 只用 python3 标准库。
# ============================================================================

import argparse
import random
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOG_LOCK = threading.Lock()
LOG_FILE = None
STATUS = 500
P_FAIL = 1.0
RNG = random.Random(20261122)      # 固定种子：同一份脚本跑两次给出同一串判定
SEQ = 0


def log(fields):
    if LOG_FILE is None:
        return
    line = f"{time.time():.6f} " + " ".join(f"{k}={v}" for k, v in fields)
    with LOG_LOCK:
        LOG_FILE.write(line + "\n")
        LOG_FILE.flush()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        pass

    def do_GET(self):
        global SEQ
        with LOG_LOCK:
            SEQ += 1
            seq = SEQ
            fail = RNG.random() < P_FAIL
        tid = threading.get_ident() % 1000
        code = STATUS if fail else 200
        body = b"boom\n" if fail else b"ok\n"
        self.send_response(code)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
        log([("ev", "req"), ("seq", seq), ("tid", tid), ("code", code),
             ("range", self.headers.get("Range") or "-")])

    def do_HEAD(self):
        # HEAD 也按同一概率失败：否则「探测成功、GET 失败」会让重试计数口径变浑
        with LOG_LOCK:
            fail = RNG.random() < P_FAIL
        self.send_response(STATUS if fail else 200)
        self.send_header("Content-Length", "1024")
        self.end_headers()
        log([("ev", "head"), ("tid", threading.get_ident() % 1000),
             ("code", STATUS if fail else 200)])


def main():
    global LOG_FILE, STATUS, P_FAIL
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8898)
    ap.add_argument("--log", default="")
    ap.add_argument("--status", type=int, default=500)
    ap.add_argument("--p-fail", type=float, default=1.0)
    a = ap.parse_args()
    STATUS, P_FAIL = a.status, a.p_fail
    if a.log:
        LOG_FILE = open(a.log, "a", buffering=1)

    ThreadingHTTPServer.allow_reuse_address = True
    ThreadingHTTPServer.daemon_threads = True
    srv = ThreadingHTTPServer(("127.0.0.1", a.port), Handler)
    sys.stdout.write(f"READY status={STATUS} p_fail={P_FAIL} port={a.port}\n")
    sys.stdout.flush()
    srv.serve_forever()


if __name__ == "__main__":
    main()
