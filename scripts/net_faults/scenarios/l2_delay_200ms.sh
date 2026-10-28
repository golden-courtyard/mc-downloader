#!/usr/bin/env bash
# NAME: l2_delay_200ms
# LAYER: l2
# NEEDS_ROOT: yes
# TIMEOUT_S: 60
# ASSERT: assert_elapsed_min -- assert_sha -- assert_local_only
# SIZE_MB: 16
#
# 场景：链路 RTT 200ms（抖动 50ms），16MB 下完并校验内容，同时看总耗时下界。
# 期望并发 = 4（16MB = 4 片，并行拉）。
# 判据：sha1 == 离线常量 且 实测总耗时 ≥ delay × 分片数 ÷ 并发 = 200 × 4 ÷ 4 = 200ms
#   200ms 这一档比 20ms 有区分度：真施加了 delay 时，任何一个 RTT 都 ≥200ms，
#   "没施加成功"时会看到 ~130ms（不加 netem 的实测基线），会被这条拦住。
set -uo pipefail

BIN="${BIN:-$HOME/mc-downloader/target/release/mc-downloader}"
export BIN

source "$(dirname "$0")/../lib.sh"
load_env

# 场景内断言：耗时下界（lib.sh 无此断言；只读 CASE_ELAPSED_MS，无副作用）
assert_elapsed_min() {
    local min_ms="${1-}"
    if [[ -z "$min_ms" ]]; then
        ASSERT_DETAIL="elapsed=unset(expect>=?)"
        return 2
    fi
    ASSERT_DETAIL="elapsed=${CASE_ELAPSED_MS}ms expect>=${min_ms}ms"
    (( CASE_ELAPSED_MS >= min_ms )) && return 0
    return 1
}

EXPECT_SHA=bd60405be79948989a6c1d6963ee20e6c940b936   # expected_sha1.sh 16
SIZE_MB=16
MIN_MS=200
NAME=l2_delay_200ms

start_origin "$SIZE_MB" "$EXPECT_SHA" || { echo "FAIL $NAME origin_failed"; exit 1; }
apply delay 200ms 50ms || { echo "FAIL $NAME apply_failed"; exit 1; }

CASE_CMD=(--piece-conns 4 fetch "http://127.0.0.1:$PORT/blob.bin" --dir "$OUT" --name t.bin)
CASE_OUT="$OUT/t.bin"
CASE_TIMEOUT_S=60
run_case "$NAME" assert_elapsed_min "$MIN_MS" -- assert_sha "$EXPECT_SHA" -- assert_local_only
rc=$?

clear
exit $rc
