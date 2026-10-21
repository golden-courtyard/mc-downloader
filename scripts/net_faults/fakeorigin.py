#!/usr/bin/env python3
# ============================================================================
# scripts/net_faults/fakeorigin.py —— netem 故障测试用的「假源」
#
# 职责：按 Range 返回字节。**不模拟任何故障**（故障只由 netem / iptables 施加，
# 见 design D3 —— 源端限速会让「实测吞吐 vs 限速值」的比对失去意义）。
#
# CLI（被 lib.sh 的 start_origin 锁定，改名会让 14 个场景全挂）：
#   python3 fakeorigin.py --size-mb N --port P [--verify SHA]
#
# 三条硬要求：
#   1) SO_REUSEADDR 必须开：上一条场景留下的 TIME_WAIT 会让 bind 直接失败，
#      表现为「启动即退出 / 端口被占」，而 ss 里只有 TIME-WAIT 没有 LISTEN —— 极易误判。
#   2) 不内置任何 sleep 限速。
#   3) 启动只打印 `SIZE=<bytes>`，**不打印 sha1**；--verify 给出时自校验，
#      不符即 exit 1（design D8：期望 sha1 必须来自离线常量，不能由本源现算后回显）。
#
# 内容规则（与 expected_sha1.sh 逐字一致，改一处必须两处一起改）：
#   第 i 个字节（全文件全局偏移，i 从 0 起）= bytes(i % 251) ⇒ 周期 251 的非均匀内容。
#
# 只用 python3 标准库。
# ============================================================================

import argparse
import hashlib
import re
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PERIOD = 251                      # ← 与 expected_sha1.sh 同一个常量
PATTERN = bytes(i % PERIOD for i in range(PERIOD))
BLOCK = PERIOD * 4096             # 1,028,096 B；251 的整数倍 ⇒ 整块前进不改变相位
BLOCK_BYTES = PATTERN * (BLOCK // PERIOD)


def make_block(start: int) -> bytes:
    """返回从全局偏移 start 开始的整块内容（长度 BLOCK，相位与 start 对齐）。

    因为 BLOCK 是 PERIOD 的整数倍，只要每次前进 BLOCK 字节，相位就不变，
    所以一个响应只需构造一次，之后反复切片写出。
    """
    phase = start % PERIOD
    pat = PATTERN if phase == 0 else PATTERN[phase:] + PATTERN[:phase]
    return pat * (BLOCK // PERIOD)


def sha1_of(size_bytes: int) -> str:
    """对 bytes(i % 251) 的前 size_bytes 个字节求 sha1（与 expected_sha1.sh 同一算法）。"""
    h = hashlib.sha1()
    left = size_bytes
    off = 0
    while left > 0:
        n = BLOCK if left >= BLOCK else left
        h.update(make_block(off)[:n])
        off += n
        left -= n
    return h.hexdigest()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "fakeorigin/1.0"
    sys_version = ""

    def log_message(self, fmt, *args):
        # 64 并发 × 每片一行会把 origin.log 冲爆；这个文件只用于诊断启动失败
        pass

    # ---- Range 解析 ------------------------------------------------------
    def _parse_range(self, total):
        """返回 (start, end, err)。err 为 None 表示可用；'invalid' / 'unsatisfiable' 为错误。"""
        rng = self.headers.get("Range")
        if not rng:
            return None, None, None
        m = re.fullmatch(r"bytes=(\d*)-(\d*)", rng.strip())
        if not m or (m.group(1) == "" and m.group(2) == ""):
            return None, None, "invalid"
        first, last = m.group(1), m.group(2)
        if first == "":
            # bytes=-N：最后 N 字节
            n = int(last)
            if n <= 0:
                return None, None, "unsatisfiable"
            return max(0, total - n), total - 1, None
        start = int(first)
        if start >= total:
            return None, None, "unsatisfiable"
        end = int(last) if last != "" else total - 1
        if end < start:
            return None, None, "invalid"
        return start, min(end, total - 1), None

    # ---- 响应 ------------------------------------------------------------
    def _send_error_range(self, code, total, reason):
        self.send_response(code)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        body = (reason + "\n").encode()
        self.send_header("Content-Length", str(len(body)))
        if code == 416:
            self.send_header("Content-Range", f"bytes */{total}")
        self.end_headers()
        if self.command != "HEAD":
            try:
                self.wfile.write(body)
            except (BrokenPipeError, ConnectionResetError):
                pass

    def _serve(self, head_only):
        total = self.server.size
        start, end, err = self._parse_range(total)

        if err == "invalid":
            self._send_error_range(400, total, "bad Range header")
            return
        if err == "unsatisfiable":
            self._send_error_range(416, total, "range not satisfiable")
            return

        if start is None:                      # 全量
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(total))
            self.send_header("Accept-Ranges", "bytes")
            self.end_headers()
            if not head_only:
                self._write_body(0, total)
            return

        length = end - start + 1
        self.send_response(206)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Length", str(length))
        self.send_header("Content-Range", f"bytes {start}-{end}/{total}")
        self.send_header("Accept-Ranges", "bytes")
        self.end_headers()
        if not head_only:
            self._write_body(start, length)

    def _write_body(self, start, length):
        block = make_block(start)
        left = length
        try:
            while left > 0:
                n = BLOCK if left >= BLOCK else left
                self.wfile.write(block[:n])
                left -= n
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            # 被测端会主动 RST（l2_rst_mid 就是造这个）：那是被测行为，不是本源故障，
            # 只吞这两种连接级异常，其他异常照常抛出来。
            return

    def do_GET(self):
        self._serve(head_only=False)

    def do_HEAD(self):
        self._serve(head_only=True)


class Server(ThreadingHTTPServer):
    daemon_threads = True
    # ⭐ TIME_WAIT 之后必须还能 bind（14 场景连跑 + --repeat 3 的命门）
    allow_reuse_address = True

    def __init__(self, addr, handler, size):
        self.size = size
        super().__init__(addr, handler)


def main():
    ap = argparse.ArgumentParser(
        description="按 Range 返回 bytes(i%%251) 的假源；不限速、不模拟故障")
    ap.add_argument("--size-mb", type=int, required=True, help="文件大小（MB）")
    ap.add_argument("--port", type=int, required=True, help="监听端口（只绑 127.0.0.1）")
    ap.add_argument("--verify", default=None, metavar="SHA1",
                    help="自校验生成结果与该 sha1 是否一致，不符即 exit 1")
    a = ap.parse_args()

    if a.size_mb <= 0:
        print(f"fakeorigin: --size-mb 必须是正数（收到 {a.size_mb}）", file=sys.stderr, flush=True)
        return 2
    total = a.size_mb * 1024 * 1024

    if a.verify:
        got = sha1_of(total)
        if got != a.verify.strip().lower():
            print(f"fakeorigin: verify failed got={got} expect={a.verify}", file=sys.stderr, flush=True)
            return 1

    try:
        srv = Server(("127.0.0.1", a.port), Handler, total)
    except OSError as e:
        # 端口被占 / 没权限：说清楚是 bind 失败，别让上层猜
        print(f"fakeorigin: bind 127.0.0.1:{a.port} 失败：{e}", file=sys.stderr, flush=True)
        return 1

    print(f"SIZE={total}", flush=True)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        srv.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
