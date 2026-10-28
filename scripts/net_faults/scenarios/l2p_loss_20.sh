#!/usr/bin/env bash
# NAME: l2p_loss_20
# LAYER: l2p
# NEEDS_ROOT: yes
# TIMEOUT_S: 120
# ASSERT: check_loss20 -- assert_local_only
# SIZE_MB: 128
# PIECE_CONNS: 32
#
# 场景：内核层随机丢包 20%。这一档**不要求下载成功** —— 20% 丢包下 32 条 TCP 连接会
#   拥塞崩溃，实测 200s 内一片 4MB 都下不完（.part 分配 0MB、state done=0）。
#   它要看的是：**失败/超时之后，有没有留下"可续传"的假象**。
#
# 判据（两条任一成立即 PASS）：
#   ① rc==0 → 下完了：sha1 必须与离线常量一致
#   ② rc!=0 → 不得留假象：把 netem 撤掉后用同一目录真的续一次，
#      能续上且最终 sha1 正确 ⇒ 留下的 state 是诚实的；否则就是假象。
#
# ⚠ 为什么必须区分「被装置杀掉」与「程序自己放弃」：
#   `download.rs:175` + `:394` 是两层重试（retries=3、退避 1+2+4s），失败前约 28-40s 都在
#   重试。本场景用 CASE_TIMEOUT_S=90 让 run_case 主动 SIGTERM，rc 会是 143 ——
#   **那是装置杀的，不是程序放弃**，所以判定行里必须标 `outcome=timeout_killed`，
#   不能写成"程序失败"。90s 也是为了不超 spec 的单场景 ≤120s。
#
# ⚠ 为什么用 `timeout` 包一层（BIN=timeout）而不是让 run_case 自己超时：
#   lib.sh 的 run_case 有个硬规则 —— **只要 CASE_TIMEOUT==1 就判 FAIL**（与断言无关）。
#   但本场景的契约是「超时也算可接受结果，只要不留假象」。两者冲突。
#   所以把"装置超时"下移成一次普通的非零退出：`timeout 90 mc-downloader …` →
#   rc=124，CASE_TIMEOUT 保持 0，最后由本场景的断言决定 PASS/FAIL。
#   （这也是一个框架缺口：`CASE_TIMEOUT_OK=1` 这类开关目前没有，已上报。）
#
# ⚠ 恢复时不调 `clear()`：lib 的 clear 会连假源一起杀（__nf_kill_origin），
#   续传就没源可下。这里只撤 qdisc，MTU / 假源 / iptables 交给场景末尾的 clear。
#
# 实测（VM）：rc=124（timeout 90s 杀），state done=0，续传 801ms，sha ok
set -uo pipefail

REAL_BIN="${BIN:-$HOME/mc-downloader/target/release/mc-downloader}"
# run_case 跑的是 `timeout 90 <真二进制> …`；真正的被测进程仍是 mc-downloader
BIN="timeout"
export BIN

source "$(dirname "$0")/../lib.sh"
load_env

EXPECT_SHA=7579901a330870a2072bfd7efec171b75c629362   # expected_sha1.sh 128
SIZE_MB=128
PIECE_CONNS=32
NAME=l2p_loss_20

# ── 断言：完成就校验；没完成就验"留下来的进度是不是诚实的" ──────────────
check_loss20() {
    local sha="$1"
    local state="$CASE_OUT.part.state"
    local done="absent"
    local d=""

    if [[ -f "$state" ]]; then
        done="$(python3 -c 'import sys; raw=open(sys.argv[1],"rb").read(); print(sum(bin(b).count("1") for b in raw[32:]))' "$state" 2>/dev/null)"
        [[ -z "$done" ]] && done="?"
    fi

    if (( CASE_RC == 0 )); then
        if ! assert_sha "$CASE_OUT" "$sha"; then
            d="$ASSERT_DETAIL"
            ASSERT_DETAIL="outcome=completed elapsed=${CASE_ELAPSED_MS}ms ${d}"
            return 1
        fi
        ASSERT_DETAIL="outcome=completed elapsed=${CASE_ELAPSED_MS}ms sha=ok"
        return 0
    fi

    local outcome="errored"
    # rc=124 是外层 `timeout 90` 的退出码 ⇒ 装置把它杀了，不是程序自己放弃
    (( CASE_RC == 124 )) && outcome="timeout_killed"

    # 只撤 netem（保假源活着），再续一次
    "${SUDO[@]}" "$TC_BIN" qdisc del dev "$IFACE" root >/dev/null 2>&1 || true

    local log2="$OUT/logs/${NAME}.resume.log"
    "$REAL_BIN" --piece-conns "$PIECE_CONNS" fetch "http://127.0.0.1:$PORT/blob.bin" \
        --dir "$OUT" --name t.bin >"$log2" 2>&1
    local rc2=$?
    local resumed=restart
    grep -q "续传：已完成" "$log2" && resumed=resume

    if (( rc2 != 0 )); then
        ASSERT_DETAIL="outcome=$outcome part_state_done=$done resume_rc=$rc2 resumed=$resumed"
        return 1
    fi
    if ! assert_sha "$CASE_OUT" "$sha"; then
        d="$ASSERT_DETAIL"
        # 这是最要紧的失败：state 说有进度，续出来却不对 ⇒ 假象
        ASSERT_DETAIL="outcome=$outcome part_state_done=$done resumed=$resumed sha=假象(${d})"
        return 1
    fi
    ASSERT_DETAIL="outcome=$outcome part_state_done=$done resumed=$resumed sha=ok"
    return 0
}

rm -f "$OUT/t.bin.part"*
start_origin "$SIZE_MB" "$EXPECT_SHA" || { echo "FAIL $NAME origin_failed"; exit 1; }
apply loss 20% || { echo "FAIL $NAME apply_failed"; exit 1; }

CASE_CMD=(90 "$REAL_BIN" --piece-conns "$PIECE_CONNS" fetch "http://127.0.0.1:$PORT/blob.bin" --dir "$OUT" --name t.bin)
CASE_OUT="$OUT/t.bin"
CASE_TIMEOUT_S=110          # 兜底：真卡住时由 run_case 杀，90s 的内层 timeout 先到
run_case "$NAME" check_loss20 "$EXPECT_SHA" -- assert_local_only
rc=$?

clear
exit $rc
