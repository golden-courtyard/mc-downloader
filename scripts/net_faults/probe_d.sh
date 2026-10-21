#!/usr/bin/env bash
# ============================================================================
# probe_d.sh —— **一次性探针**（不进 run_all，也不放在 scenarios/ 下）
#
# 目的：结掉一个悬案 —— 「1 连（管子本来就吃满）下，tuner 有没有可测的吞吐贡献？」
#   背景：这个对照**不能常驻场景**，因为"降速后收敛"在任何一对臂上都没有可测落点
#         （见 aimd_step.sh 文件头"为什么不再加第 4 臂"）。
#         所以做成一次性探针：花 53s 结案，不必每轮 CI 常驻。
#
# 配置：1 连 + `--adaptive=false`（无 tuner），53s（seg1 25s + seg2 到 50s 窗口关闭）
#
# 预测（机制推导，implementer-2）：
#   D:m ≈ A:m（差 <2%）、D:k ≈ A:k
#   理由：单流在 8mbit 上本来就能吃满；零 tick 来自换挡瞬态，与连接数无关。
#
# 证伪条件：
#   · D:m 比 A:m 低 **>2%**            ⇒ 推断错，1 连下 tuner 确有可测贡献
#   · D 出现 **t > 34s** 的中段零 tick  ⇒ 段内停摆不只由并发数决定，另有机制，要查
#   · D 的零只在 **≈32–33s**（窗口边界）⇒ 相位运气（单连只有 1 次 RTO 相位采样），不算反证
#
# ⚠️ **必须记零 tick 的时间戳，不能只记 count**：
#    窗口边界上的零（相位运气）与段中的零（另有机制）是两件事，count 分不开。
#
# ⚠️ 跑完把结论写回本文件头，**然后本文件可以删** —— 别让它变成第二个"挂着没人跑"的场景。
#
# ── 实测结论（2026-10-21，VM）────────────────────────────────────────────
#   （待填）
# ============================================================================
set -uo pipefail

BIN="${BIN:-$HOME/mc-downloader/target/release/mc-downloader}"
export BIN

source "$(dirname "$0")/lib.sh"
load_env

SHA1G=e725338708d1ba48048f65b3fb38798461bdbf27   # expected_sha1.sh 1024
ARM_S=53                                          # seg1 25s + seg2 窗口到 50s + 余量
TARGET_MS=25000
# A 臂基线（1 连 + tuner）：来自 ebbb085 那轮 aimd_step 的 A:m，作为对比基准
A_M_BASELINE="${A_M_BASELINE:-8075303}"
A_K_BASELINE="${A_K_BASELINE:-2}"

start_origin 1024 "$SHA1G" || { echo "probe_d: origin_failed" >&2; exit 1; }
apply rate 80mbit || { echo "probe_d: apply_failed" >&2; exit 1; }

csv="$OUT/logs/probe_d.csv"
rm -rf "$OUT/probe_d"

bash "$__NF_DIR/trace_rows.sh" "$csv" -- \
    timeout "$ARM_S" "$BIN" --adaptive=false --piece-conns 1 \
    fetch "http://127.0.0.1:$PORT/blob.bin" --dir "$OUT/probe_d" --name t.bin &
dp=$!

# 按绝对时刻换挡（与场景 run_arm 同形，避免 sleep 累积漂移）
t0="$(date +%s%N)"
while :; do
    (( ($(date +%s%N) - t0) / 1000000 >= TARGET_MS )) && break
    sleep 0.2
done
apply rate 8mbit || true
wait "$dp" 2>/dev/null
clear

# ── 统计（与场景同一窗口：seg2 = 32–50s）───────────────────────────────────
stats="$(awk -F, '
    NR == 1 { next }
    $2 == "tick" && $3 != "" {
        t = $1 + 0; v = $3 + 0
        if (t >= 32000 && t < 50000) { s2 += v; n2++; if (v == 0) { z2++; zts = zts sprintf("%.1f ", t/1000) } }
    }
    END { printf "seg2=%.0f n=%d k=%d zts=%s\n", (n2 ? s2/n2 : 0), n2, z2, (zts == "" ? "-" : zts) }
' "$csv")"
seg2="$(sed -n 's/.*seg2=\([0-9]*\).*/\1/p' <<<"$stats")"
n="$(sed -n 's/.*[^a-z]n=\([0-9]*\).*/\1/p' <<<"$stats")"
k="$(sed -n 's/.*k=\([0-9]*\).*/\1/p' <<<"$stats")"
zts="$(sed -n 's/.*zts=\(.*\)$/\1/p' <<<"$stats")"
m="$(awk -v s="${seg2:-0}" -v nn="${n:-0}" -v kk="${k:-0}" \
    'BEGIN{ d = nn - kk; if (d > 0) printf "%.0f", s*nn/d; else printf "0" }')"

echo "=== probe_d 结果（1 连 + 无 tuner，${ARM_S}s）==="
echo "  D: seg2=$seg2  n=$n  k=$k  m=$m"
echo "  零 tick 时间戳: $zts"
echo "  A 基线: m=$A_M_BASELINE  k=$A_K_BASELINE（来自 aimd_step 的 A 臂）"
echo
# ── 按证伪条件自动判读 ─────────────────────────────────────────────────────
awk -v m="$m" -v am="$A_M_BASELINE" -v k="$k" -v zts="$zts" -v nom=8000000 'BEGIN{
    diff = (am > 0) ? (m - am) / am * 100 : 0
    printf "  D:m vs A:m = %+.2f%%\n", diff
    # 先做物理合理性检查：netem 是限速上限，持续吞吐不可能长期 > 标称
    if (m > nom * 1.02) {
        printf "  ⚠️ m=%.0f 是标称的 %.1f%% —— **m 指标本轮失真**（长停摆后的补发脉冲把非零 tick 抬高）\n", m, m*100/nom
        print "  ⇒ 结论：**本探针测不了**。要用 m 判 tuner 贡献，必须先消除补发脉冲（改用中位数或截顶）"
        exit
    }
    if (diff < -2)
        print "  ⇒ 证伪①：D 比 A 低 >2% ⇒ 1 连下 tuner 确有可测贡献（推断错）"
    else if (diff > 2)
        printf "  ⇒ 注意：D 比 A **高** %+.2f%% —— 与预测方向相反，先查是不是补发/采样问题，别当成结论\n", diff
    else
        print "  ⇒ 证伪①未触发：|差| ≤ 2% ⇒ 1 连下 tuner 对吞吐无可测贡献"
    # 段中零 tick（>34s）
    n_mid = 0
    if (zts != "-") {
        cnt = split(zts, arr, " ")
        for (i = 1; i <= cnt; i++) if (arr[i] + 0 > 34) n_mid++
    }
    if (n_mid > 0)
        printf "  ⇒ 证伪②命中：有 %d 个 t>34s 的段中零 tick ⇒ 段内停摆不只由并发数决定，另有机制\n", n_mid
    else
        print "  ⇒ 证伪②未触发：无段中零 tick"
    if (k > 0 && n_mid == 0)
        print "  ⇒ 零 tick 仅在窗口边界 ⇒ 相位运气（单连 1 次 RTO 相位采样），不算反证"
}'
