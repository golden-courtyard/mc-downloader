#!/usr/bin/env bash
# ============================================================================
# scripts/net_faults/expected_sha1.sh —— 离线预算 bytes(i%251) 各尺寸的 sha1
#
# 为什么必须离线算（design D8）：
#   若让 fakeorigin.py 启动时现算 sha1 再比对，那么「生成端写错」与「据此算出的
#   sha1」会自洽通过 —— 两侧同错，什么都测不出来。所以这份常量必须由**另一条
#   代码路径**一次算出、写死进文件，运行时只做查表比对。
#
# 用法：
#   bash expected_sha1.sh               # 生成 expected_sha1.txt（16MB / 128MB / 1GB）
#   bash expected_sha1.sh 16            # 只打印 16MB 档的 sha1（独立于 fakeorigin 算出）
#   bash expected_sha1.sh 16 128        # 打印多档，每行 "<mb> <sha1>"
#
# 输出文件格式（场景用 `grep '^16' expected_sha1.txt` 取用，行首不能有空格）：
#   # size_mb  bytes  sha1
#   16        16777216        3f0a...
#   128       134217728       ...
#   1024      1073741824      ...
#
# 内容规则（与 fakeorigin.py 逐字一致，改一处必须两处一起改）：
#   第 i 个字节（i 从 0 起，全文件全局偏移）= bytes(i % 251)
#   ⇒ 周期 251 的非均匀内容。均匀内容无法检出写偏移错误（spec §6.2）。
# ============================================================================
set -euo pipefail

__HERE="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

python3 - "$__HERE" "$@" <<'PY'
import hashlib
import sys

PERIOD = 251                      # ← 与 fakeorigin.py 同一个常量
PATTERN = bytes(i % PERIOD for i in range(PERIOD))
BLOCK = PERIOD * 4096             # 1,028,096 B；251 的整数倍 ⇒ 每块都从 phase 0 开始
assert BLOCK % PERIOD == 0

OUT_NAME = "expected_sha1.txt"
TIERS_MB = (16, 128, 1024)        # 16MB / 128MB / 1GB（design §5 的三档）


def sha1_of(size_bytes: int) -> str:
    """对 bytes(i % 251) 的前 size_bytes 个字节求 sha1。"""
    h = hashlib.sha1()
    block = PATTERN * (BLOCK // PERIOD)
    left = size_bytes
    while left > 0:
        n = BLOCK if left >= BLOCK else left
        h.update(block[:n])
        left -= n
    return h.hexdigest()


def main() -> int:
    here = sys.argv[1]
    want = [int(a) for a in sys.argv[2:]]
    for mb in want:
        if mb <= 0:
            print(f"expected_sha1.sh: 尺寸必须是正数（收到 {mb}）", file=sys.stderr)
            return 2
        print(f"{mb} {sha1_of(mb * 1024 * 1024)}")
    if want:
        return 0

    path = f"{here}/{OUT_NAME}"
    rows = [(mb, mb * 1024 * 1024, sha1_of(mb * 1024 * 1024)) for mb in TIERS_MB]
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write("# size_mb\tbytes\tsha1   （规则 bytes(i%251)，离线预算，见 expected_sha1.sh）\n")
        for mb, nbytes, digest in rows:
            f.write(f"{mb}\t{nbytes}\t{digest}\n")
    print(f"written: {path}")
    for mb, nbytes, digest in rows:
        print(f"  {mb:>5} MB  {nbytes:>11} B  {digest}")
    return 0


sys.exit(main())
PY
