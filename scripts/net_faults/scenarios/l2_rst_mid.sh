#!/usr/bin/env bash
# NAME: l2_rst_mid
# LAYER: l2
# NEEDS_ROOT: yes
# TIMEOUT_S: 120
# ASSERT: check_rst -- assert_local_only
# SIZE_MB: 16
#
# 场景：下载到约 50% 时从另一进程注入 REJECT --reject-with tcp-reset，测被测端在
#   「进程还活着」时的 Err 分支：要么在重试预算内恢复并完成，要么报错退出但把进度
#   留在 .part.state 里，且下次能续上。
#
# ⚠ 三处与任务书表格不同的地方，都是实测逼出来的（见报告）：
#
# 1) 必须加 `rate 50mbit`（任务书写的是"无 netem"）。
#    不加 netem 时 16MB 只要 **130ms**，50% 落在 65ms 处；而 iptables 加一条规则的
#    往返就有 10-30ms，"打到一半"无法可靠复现 —— 注入晚了就等于没测。加 50mbit 后
#    传输窗口 ≈2.6s，注入点有 1s 级余量。
#
# 2) 必须 `--piece-conns 2`（任务书写的是 4）。
#    4 连时 4 个 4MB 片是**对称**的：4 条连接同时完成，.part 直到最后一刻才落盘
#    （实测 du 全程 0）。那样在 50% 注入会导致"一片都没完成"，判据②要求的
#    「.part.state 记录的分片数 > 0」永远不成立。
#    2 连时片按序完成：50% 时已有 2 片落盘（实测 du=8192KB），判据②可判。
#
# 3) 判据②里的「能续上」不能只比 sha1 —— 从头重下也能得到正确 sha1。
#    正判据是第二轮日志出现「续传：已完成 N/M 片，只补缺口」，且 N > 0。
#
# 判据（两条任一成立即 PASS，但证据必须齐）：
#   ① rc==0：说明重试预算内恢复了 → 要求 sha1 == 离线常量，且注入确实发生在下载中途
#   ② rc!=0：要求 .part.state 存在且已完成片数 > 0，且第二轮能续上并 sha1 一致
set -uo pipefail

BIN="${BIN:-$HOME/mc-downloader/target/release/mc-downloader}"
export BIN

source "$(dirname "$0")/../lib.sh"
load_env

EXPECT_SHA=bd60405be79948989a6c1d6963ee20e6c940b936   # expected_sha1.sh 16
SIZE_MB=16
NAME=l2_rst_mid
PART="$OUT/t.bin.part"
STATE="$PART.state"
INJECT_FILE="$OUT/.rst_injected"

# ── 断言：两条判据 ────────────────────────────────────────────────────────
check_rst() {
    local sha="$1"
    local rst_ms="" rst_kb=""
    [[ -f "$INJECT_FILE" ]] && read -r rst_ms rst_kb <"$INJECT_FILE"

    # 注入证据是所有分支的前提：没注入过的样本判不了，直接 FAIL（别让它蒙过）
    if [[ -z "$rst_ms" ]]; then
        ASSERT_DETAIL="rst=never_injected(下载太快，50% 前就结束了)"
        return 1
    fi

    if (( CASE_RC == 0 )); then
        # ① 恢复了
        local d=""
        if ! assert_sha "$CASE_OUT" "$sha"; then
            d="$ASSERT_DETAIL"
            ASSERT_DETAIL="recovered=yes rst_at=${rst_ms}ms rst_kb=${rst_kb} ${d}"
            return 1
        fi
        ASSERT_DETAIL="recovered=yes rst_at=${rst_ms}ms rst_kb=${rst_kb} elapsed=${CASE_ELAPSED_MS}ms sha=ok"
        return 0
    fi

    # ② 报错退出：progress 必须留在 state 里，且能续上
    if [[ ! -f "$STATE" ]]; then
        ASSERT_DETAIL="rc=$CASE_RC part_state=absent(expect=present) rst_at=${rst_ms}ms"
        return 1
    fi
    local done
    done="$(python3 -c 'import sys; raw=open(sys.argv[1],"rb").read(); print(sum(bin(b).count("1") for b in raw[32:]))' "$STATE" 2>/dev/null)"
    [[ -z "$done" ]] && done=-1
    if (( done <= 0 )); then
        ASSERT_DETAIL="rc=$CASE_RC part_state=present(done=$done,expect>0) rst_at=${rst_ms}ms"
        return 1
    fi

    # 规则此时已撤（注入器只保持 600ms），再跑一次看能不能续上
    local log2="$OUT/logs/${NAME}.resume.log"
    "$BIN" --piece-conns 2 fetch "http://127.0.0.1:$PORT/blob.bin" --dir "$OUT" --name t.bin >"$log2" 2>&1
    local rc2=$?
    local resumed=no
    grep -q "续传：已完成" "$log2" && resumed=yes

    if (( rc2 != 0 )); then
        ASSERT_DETAIL="rc=$CASE_RC resume_rc=$rc2 resumed=$resumed part_state=present(done=$done)"
        return 1
    fi
    if ! assert_sha "$CASE_OUT" "$sha"; then
        ASSERT_DETAIL="rc=$CASE_RC resumed=$resumed done=$done sha_bad"
        return 1
    fi
    ASSERT_DETAIL="rc=$CASE_RC part_state=present(done=$done) resumed=$resumed rst_at=${rst_ms}ms sha=ok"
    return 0
}

# ── 准备 ──────────────────────────────────────────────────────────────────
rm -f "$INJECT_FILE" "$OUT/t.bin.part" "$OUT/t.bin.part.state"
start_origin "$SIZE_MB" "$EXPECT_SHA" || { echo "FAIL $NAME origin_failed"; exit 1; }
apply rate 50mbit || { echo "FAIL $NAME apply_failed"; exit 1; }

# ── 注入器：轮询 .part 的已分配字节，过半就加 RST 规则；保持 600ms 后撤掉 ──
# 用 du 而不是日志进度：日志 1s 才一行，会错过注入窗口；du 反映真实落盘量。
(
    total_kb=$((SIZE_MB * 1024))
    t0=$(date +%s%N)
    while :; do
        now=$(date +%s%N)
        (( (now - t0) / 1000000 > 30000 )) && break
        kb="$(du -k -- "$PART" 2>/dev/null | awk '{print $1}')"
        kb=${kb:-0}
        if (( kb * 2 >= total_kb )); then
            if sudo -n "$IPT_BIN" -A OUTPUT -p tcp --sport "$PORT" -j REJECT \
                --reject-with tcp-reset 2>/dev/null; then
                printf '%s %s\n' "$(( (now - t0) / 1000000 ))" "$kb" >"$INJECT_FILE"
                sleep 0.6
                sudo -n "$IPT_BIN" -D OUTPUT -p tcp --sport "$PORT" -j REJECT \
                    --reject-with tcp-reset 2>/dev/null || true
            fi
            break
        fi
        sleep 0.01
    done
) &
INJECTOR=$!

# ── 跑一遍 ────────────────────────────────────────────────────────────────
CASE_CMD=(--piece-conns 2 fetch "http://127.0.0.1:$PORT/blob.bin" --dir "$OUT" --name t.bin)
CASE_OUT="$OUT/t.bin"
CASE_TIMEOUT_S=60
run_case "$NAME" check_rst "$EXPECT_SHA" -- assert_local_only
rc=$?

# 注入器可能还在 sleep 0.6s，收掉它；规则再删一次（幂等，删不到也无妨）
kill "$INJECTOR" 2>/dev/null || true
wait "$INJECTOR" 2>/dev/null || true
sudo -n "$IPT_BIN" -D OUTPUT -p tcp --sport "$PORT" -j REJECT \
    --reject-with tcp-reset 2>/dev/null || true

clear
exit $rc
