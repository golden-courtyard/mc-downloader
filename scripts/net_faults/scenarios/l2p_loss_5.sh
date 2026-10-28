#!/usr/bin/env bash
# NAME: l2p_loss_5
# LAYER: l2p
# NEEDS_ROOT: yes
# TIMEOUT_S: 60
# ASSERT: assert_sha -- assert_local_only
# SIZE_MB: 128
# PIECE_CONNS: 32
#
# 场景：内核层随机丢包 5%，128MB / 32 连完整下载后校验内容。
# 期望并发 = 约 27-36（128MB = 32 片；峰值不写死）。
# 判据：sha1 == 离线常量（5% 丢包仍在 TCP 重传能兜住的范围内）
# 实测（VM）：rc=0，1288ms，sha ok
set -uo pipefail

BIN="${BIN:-$HOME/mc-downloader/target/release/mc-downloader}"
export BIN

source "$(dirname "$0")/../lib.sh"
load_env

EXPECT_SHA=7579901a330870a2072bfd7efec171b75c629362   # expected_sha1.sh 128
SIZE_MB=128
PIECE_CONNS=32
NAME=l2p_loss_5

# 清掉上一轮的 .part/.part.state：否则下一轮会"续传"，把本轮该下的量测少
rm -f "$OUT/t.bin.part"*
start_origin "$SIZE_MB" "$EXPECT_SHA" || { echo "FAIL $NAME origin_failed"; exit 1; }
apply loss 5% || { echo "FAIL $NAME apply_failed"; exit 1; }

CASE_CMD=(--piece-conns "$PIECE_CONNS" fetch "http://127.0.0.1:$PORT/blob.bin" --dir "$OUT" --name t.bin)
CASE_OUT="$OUT/t.bin"
CASE_TIMEOUT_S=50
run_case "$NAME" assert_sha "$EXPECT_SHA" -- assert_local_only
rc=$?

clear
exit $rc
