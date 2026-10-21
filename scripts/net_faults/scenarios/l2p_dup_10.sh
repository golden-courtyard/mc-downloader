#!/usr/bin/env bash
# NAME: l2p_dup_10
# LAYER: l2p
# NEEDS_ROOT: yes
# TIMEOUT_S: 60
# ASSERT: assert_sha
# SIZE_MB: 128
# PIECE_CONNS: 32
#
# 场景：内核层重复包 10%。TCP 本身应完成去重，所以这一档考的是**我们自己**：
#   若 sha1 不一致（文件变长/字节错位），说明我们的 `expect` 长度校验或偏移写有问题
#   —— 不去重就会把重复字节也写进去。
# 期望并发 = 约 27-36（128MB = 32 片）。
# 判据：sha1 == 离线常量。
# 实测（VM）：rc=0，298ms，sha ok
set -uo pipefail

BIN="${BIN:-$HOME/mc-downloader/target/release/mc-downloader}"
export BIN

source "$(dirname "$0")/../lib.sh"
load_env

EXPECT_SHA=7579901a330870a2072bfd7efec171b75c629362   # expected_sha1.sh 128
SIZE_MB=128
PIECE_CONNS=32
NAME=l2p_dup_10

rm -f "$OUT/t.bin.part"*
start_origin "$SIZE_MB" "$EXPECT_SHA" || { echo "FAIL $NAME origin_failed"; exit 1; }
apply duplicate 10% || { echo "FAIL $NAME apply_failed"; exit 1; }

CASE_CMD=(--piece-conns "$PIECE_CONNS" fetch "http://127.0.0.1:$PORT/blob.bin" --dir "$OUT" --name t.bin)
CASE_OUT="$OUT/t.bin"
CASE_TIMEOUT_S=50
run_case "$NAME" assert_sha "$EXPECT_SHA"
rc=$?

clear
exit $rc
