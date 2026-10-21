#!/usr/bin/env bash
# NAME: l2_rate_8mbit
# LAYER: l2
# NEEDS_ROOT: yes
# TIMEOUT_S: 60
# ASSERT: assert_rate -- assert_sha
# SIZE_MB: 16
#
# 场景：链路限速 8mbit，落盘后校验内容与吞吐。
# 期望并发 = 4：16MB / 4MB 片 = 4 片，`--piece-conns 4` 刚好打满（再多也用不上）。
#   显式写 4 是为了说明"知道上限在哪"，不是抄默认值。
# 判据：sha1 == 离线常量 且 实测吞吐 ∈ [0.8, 1.2] × 8000000 bps
set -uo pipefail

BIN="${BIN:-$HOME/mc-downloader/target/release/mc-downloader}"
export BIN

source "$(dirname "$0")/../lib.sh"
load_env

EXPECT_SHA=bd60405be79948989a6c1d6963ee20e6c940b936   # expected_sha1.sh 16
SIZE_MB=16
TARGET_BPS=8000000
NAME=l2_rate_8mbit

start_origin "$SIZE_MB" "$EXPECT_SHA" || { echo "FAIL $NAME origin_failed"; exit 1; }
apply rate 8mbit || { echo "FAIL $NAME apply_failed"; exit 1; }

CASE_CMD=(--piece-conns 4 fetch "http://127.0.0.1:$PORT/blob.bin" --dir "$OUT" --name t.bin)
CASE_OUT="$OUT/t.bin"
CASE_TIMEOUT_S=60
run_case "$NAME" assert_rate "$TARGET_BPS" -- assert_sha "$EXPECT_SHA"
rc=$?

clear
exit $rc
