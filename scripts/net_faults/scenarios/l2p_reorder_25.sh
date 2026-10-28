#!/usr/bin/env bash
# NAME: l2p_reorder_25
# LAYER: l2p
# NEEDS_ROOT: yes
# TIMEOUT_S: 60
# ASSERT: assert_sha -- assert_local_only
# SIZE_MB: 128
# PIECE_CONNS: 32
#
# 场景：内核层乱序 25%（配 10ms 延迟；netem 的 reorder 必须搭 delay 才生效）。
#   25% 比 10% 档更容易让同一个 TCP 流内的段乱序，从而压出"偏移写错"类缺陷。
# 判据：sha1 == 离线常量。**不一致即为真缺陷**（我们的偏移算法在并发写时错了）。
# 期望并发 = 约 27-36（128MB = 32 片）。
# 实测（VM）：rc=0，4060ms，sha ok
set -uo pipefail

BIN="${BIN:-$HOME/mc-downloader/target/release/mc-downloader}"
export BIN

source "$(dirname "$0")/../lib.sh"
load_env

EXPECT_SHA=7579901a330870a2072bfd7efec171b75c629362   # expected_sha1.sh 128
SIZE_MB=128
PIECE_CONNS=32
NAME=l2p_reorder_25

rm -f "$OUT/t.bin.part"*
start_origin "$SIZE_MB" "$EXPECT_SHA" || { echo "FAIL $NAME origin_failed"; exit 1; }
apply delay 10ms reorder 25% || { echo "FAIL $NAME apply_failed"; exit 1; }

CASE_CMD=(--piece-conns "$PIECE_CONNS" fetch "http://127.0.0.1:$PORT/blob.bin" --dir "$OUT" --name t.bin)
CASE_OUT="$OUT/t.bin"
CASE_TIMEOUT_S=50
run_case "$NAME" assert_sha "$EXPECT_SHA" -- assert_local_only
rc=$?

clear
exit $rc
