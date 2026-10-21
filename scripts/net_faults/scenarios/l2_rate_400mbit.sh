#!/usr/bin/env bash
# NAME: l2_rate_400mbit
# LAYER: l2
# NEEDS_ROOT: yes
# TIMEOUT_S: 60
# ASSERT: assert_rate -- assert_sha
# SIZE_MB: 128
#
# 场景：链路限速 400mbit，落盘后校验内容与吞吐。
# 期望并发 = 4（128MB = 32 片，`--piece-conns 4` 打不满但够用；本档只看吞吐）。
#
# ⚠ 为什么这一档用 128MB 而不是 16MB（实测逼出来的改动）：
#   netem `rate 400mbit` 在 lo 上的**实际能力只有 375-383 Mbps**（约 95%，与包开销/
#   MTU 1500 的记账有关）。16MB 在 400mbit 下的窗口只有 ~400ms，其中约 90ms 是进程
#   启动 + 长度探测，占比 20% —— 实测吞吐因此掉到 287/324 Mbps（71.7%/81.0%），
#   在下界 320 Mbps 附近来回抖，判据会随机红。
#   换成 128MB 后窗口 2.9s，启动开销摊薄：实测 374 Mbps（93.5%，与 shaper 能力一致）。
#   ⇒ 原则：判据的观测窗口必须远大于被测程序的固定开销，否则测到的是开销不是带宽。
#
# 判据：sha1 == 离线常量 且 实测吞吐 ∈ [0.8, 1.2] × 400000000 bps
set -uo pipefail

BIN="${BIN:-$HOME/mc-downloader/target/release/mc-downloader}"
export BIN

source "$(dirname "$0")/../lib.sh"
load_env

EXPECT_SHA=7579901a330870a2072bfd7efec171b75c629362   # expected_sha1.sh 128
SIZE_MB=128
TARGET_BPS=400000000
NAME=l2_rate_400mbit

start_origin "$SIZE_MB" "$EXPECT_SHA" || { echo "FAIL $NAME origin_failed"; exit 1; }
apply rate 400mbit || { echo "FAIL $NAME apply_failed"; exit 1; }

CASE_CMD=(--piece-conns 4 fetch "http://127.0.0.1:$PORT/blob.bin" --dir "$OUT" --name t.bin)
CASE_OUT="$OUT/t.bin"
CASE_TIMEOUT_S=60
run_case "$NAME" assert_rate "$TARGET_BPS" -- assert_sha "$EXPECT_SHA"
rc=$?

clear
exit $rc
