#!/usr/bin/env bash
# NAME: l2_rate_800kbit
# LAYER: l2
# NEEDS_ROOT: yes
# TIMEOUT_S: 120
# ASSERT: assert_rate -- assert_sha -- assert_local_only
# SIZE_MB: 8
#
# 场景：链路限速 800kbit，落盘后校验内容与吞吐。
#
# 为什么是 8MB 而不是 16MB（已裁决）：
#   16MB / 800kbit(≈100KB/s) ≈ 160s，超 spec 的 timeout_s ≤ 120。
#   8MB 恰在分片边界上（piece.rs: NO_SPLIT_BELOW=8MB，`<` 才不分片）→ 2 片 → ≈80s。
# 期望并发 = 2（不是名义上的 piece-conns）：8MB / 4MB 片 = 2 片，实测峰值 ESTAB=3
#   （2 条分片连接 + 1 条探测连接）。判据按"2 连吃满 800kbit"推，不按 4 连。
#
# 判据：sha1 == 离线常量 且 实测吞吐 ∈ [0.8, 1.2] × 800000 bps
set -uo pipefail

BIN="${BIN:-$HOME/mc-downloader/target/release/mc-downloader}"
export BIN

source "$(dirname "$0")/../lib.sh"
load_env

# 离线预算的常量（expected_sha1.sh 8），**不许在场景里现算**（design D8）
EXPECT_SHA=31180b8383f3358204949a0e0866abbde01f8fca
SIZE_MB=8
TARGET_BPS=800000
NAME=l2_rate_800kbit

start_origin "$SIZE_MB" "$EXPECT_SHA" || { echo "FAIL $NAME origin_failed"; exit 1; }
apply rate 800kbit || { echo "FAIL $NAME apply_failed"; exit 1; }

CASE_CMD=(--piece-conns 2 fetch "http://127.0.0.1:$PORT/blob.bin" --dir "$OUT" --name t.bin)
CASE_OUT="$OUT/t.bin"
CASE_TIMEOUT_S=110
run_case "$NAME" assert_rate "$TARGET_BPS" -- assert_sha "$EXPECT_SHA" -- assert_local_only
rc=$?

clear
exit $rc
