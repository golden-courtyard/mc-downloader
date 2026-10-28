#!/usr/bin/env bash
# NAME: l2p_reorder_10
# LAYER: l2p
# NEEDS_ROOT: yes
# TIMEOUT_S: 60
# ASSERT: assert_sha -- assert_local_only
# SIZE_MB: 128
# PIECE_CONNS: 32
#
# 场景：内核层乱序 10%（配 10ms 延迟作为重排基础 —— netem 的 reorder 单写不生效）。
#   32 条连接并发写同一个 .part 的不同偏移，乱序会让"按偏移写"的算法暴露问题。
# 判据：sha1 == 离线常量。**不一致就说明我们的偏移算法在并发写时错了** ——
#   这是本批的核心探测器（乱序不影响总字节数，但会影响到达顺序）。
# 期望并发 = 约 27-36（128MB = 32 片）。
# 实测（VM）：rc=0，2506ms，sha ok
set -uo pipefail

BIN="${BIN:-$HOME/mc-downloader/target/release/mc-downloader}"
export BIN

source "$(dirname "$0")/../lib.sh"
load_env

EXPECT_SHA=7579901a330870a2072bfd7efec171b75c629362   # expected_sha1.sh 128
SIZE_MB=128
PIECE_CONNS=32
NAME=l2p_reorder_10

rm -f "$OUT/t.bin.part"*
start_origin "$SIZE_MB" "$EXPECT_SHA" || { echo "FAIL $NAME origin_failed"; exit 1; }
apply delay 10ms reorder 10% || { echo "FAIL $NAME apply_failed"; exit 1; }

CASE_CMD=(--piece-conns "$PIECE_CONNS" fetch "http://127.0.0.1:$PORT/blob.bin" --dir "$OUT" --name t.bin)
CASE_OUT="$OUT/t.bin"
CASE_TIMEOUT_S=50
run_case "$NAME" assert_sha "$EXPECT_SHA" -- assert_local_only
rc=$?

clear
exit $rc
