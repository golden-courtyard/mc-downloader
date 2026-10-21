#!/usr/bin/env bash
# NAME: aimd_step
# LAYER: l2
# NEEDS_ROOT: yes
# TIMEOUT_S: 300
# ASSERT: check_aimd
# SIZE_MB: 1024
# PIECE_CONNS: 1 (A) / 16 (B)
#
# 场景：80mbit → 8mbit → 80mbit 三段阶跃（各 25s），看 --adaptive 的 AIMD 是否真的双向有效。
#
# ⚠ 为什么是**三臂**而不是一臂：同一个参数无法同时证两个命题（"别用参数凑判据"的反面）。
#
#   | 臂 | 参数 | 命题 | 现判据 |
#   | A | --piece-conns 1  | AIMD **双向**有效（机制存在）| cut ≥ 1、add ≥ 1、seg2 ∈ ±20%、seg3 > 2×seg2 |
#   | B | --piece-conns 16 | 高并发+慢链路 ⇒ **应退回固定并发并告警**（39765be 的守卫）| 告警 1 次且含 coarse_grain；disabled ≥ 1；add=0 且 cut=0；seg3 > 2×seg2 |
#   | C | 16 连 + --adaptive=false | 对照（钩子确实没在跑）| tune 行 = 0；seg2/max 仅诊断 |
#
#   ⚠ **A 臂的 `cut ≥ 1` 依赖流量抖动**：`cut` 的条件是 `delta < last_delta`。
#      恒定速率源（delta 每 tick 精确相等）会一直走 `hold` ⇒ **cut = 0**。
#
#   ⚠ **守卫是「一次性判定，只看起步速率」**（39765be 起）：
#       `piece_secs = piece_size × conns ÷ 总吞吐`，> 3 个 2s tick（6s）就
#       `act=disabled reason=coarse_grain` 并退回固定并发，此后不再 add/cut。
#       · 起步快、后段降速的（A 臂：起步 80mbit 通过，8mbit 段单片 16s）⇒ 守卫**不复查**，仍跑 AIMD
#       · 起步慢的（B 臂：16MB 片 × 16 连 @80mbit = **25.6s**）⇒ 一开始就退回
#       这是有意的取舍（避免段中途翻转），**不是缺陷**。
#
#       门槛参考（16 连）：16MB 片（≥256MB 素材）需总带宽 ≈ **380 mbit**
#         （标称算出来是 340，实测有效吞吐比标称低约 12% ⇒ 340~380 之间会出现"文档说够、实际退回"）
#       4MB 片（8–256MB 素材）≈ 85 mbit。
#       ⚠ 素材大小决定片大小：本场景用 1GB ⇒ **16MB/片**，别按 4MB 估。
#
#   ⚠ **`[tune]` 行格式也随 39765be 变了**：旧 `act= avail= delta= last=`，
#       新 `act= budget= take= inflight= delta= last=`（`avail` 在新架构里**不存在**）。
#       `trace_rows.sh` 按**字段名**匹配，两版都能解析；CSV 列已同步为 budget/take/inflight。
#      实测：1 MB/s 恒定源 cut=0；同一源加 ±50% 抖动后 cut=1。
#      ⇒ **用恒定源复跑 A 臂会看到 cut=0，那不是回归**，是测法丢掉了触发条件。
#
# ── 被去掉的判据 + 它为什么会误判（改判据必须留死因，否则知识就丢了）──────────
#
# ① 原判据：`B 的 seg2 ∈ [0.8, 1.2] × 8mbit`
#    死因：16 连在 8mbit 段是**双峰信号**，不是围绕均值的抖动。
#    实据：tick 序列 `7.5M → 4.2M → 0 → 0 → 2.5M → 7.5M`，8mbit 窗口内 **4/23 个 tick 为零吞吐**；
#          均值 5.9~6.8M（74~85%）越下界。**用均值判抖动 = 测错对象**。
#    现判据：只判「add≥1 且 seg3 > 2×seg2」，把 seg2 与 zero2 写进判定行作诊断。
#
# ② 原判据：`C 的 max ≤ piece_conns × 8mbit`（=128 Mbps）
#    死因：`max` 是**回补脉冲**的峰值，不是持续速率。
#    实据：max=194 Mbps @ t=64.4s，而它**前面连续 5 个 tick 为 0**（8mbit 段把 16 条流压死后
#          TCP 停摆，恢复时把内核队列积压一次性送出，单 tick ≈ 24MB）。**用 max 判持续速率 = 测错对象**。
#    现判据：只硬判「tune 行 = 0」，seg2/max 作诊断。
#
# ③ 原判据：`B 的 cut 预期 0`
#    死因：**窗口长度决定结论**，不是代码属性。
#    实据：同一份代码、同一个 conns=16 —— 25s 窗口（≈12 tick）cut=0；本场景 78s 窗口（≈38 tick）cut=1~4。
#    机制（implementer-2 给出，我用两组数据验过）：
#         permits = conns + 净add；workers = 2×conns ⇒ avail = max(0, 净add − conns)
#         ⇒ avail ≥ 2（cut 的前提）需要 **净 add ≥ conns + 2**
#         ⚠ 旧版 avail 是「本 tick 的 add 之前」读的，故打印值比累计值晚一 tick。
#           该机制已随 avail 一起消失（39765be 起 adaptive 改用 budget 闸门）—— 下方 ④ 有说明。
#           **不写行号**：那个坐标在 39765be 之后指向分片重试退避代码，
#           留着就是一个"看起来能跳、跳过去是别的东西"的链接。
#         代 tick 预算：要看到 cut，窗口 tick 数须 ≥ **conns + 3**
#           （前 conns+2 个 tick 连续 add 把 avail 顶到 2，第 conns+3 个 tick 再遇 delta 下降）
#           ⇒ 25s（≈12 tick）下 conns ≤ 9；78s（≈38 tick）下 conns ≤ 35。
#           B 臂取 conns=16 对应 19 tick ≤ 38，有余量；25s 探针取 16 是**结构性不可能**。
#         ⚠ 该条件是**必要不充分**：还需期间出现一次 delta 下降。实测 conns=4 / 12 tick 时
#           tick 够（7≤12）但只拿到 6 次 add（正好 = conns+2），没有后续下降 ⇒ cut 仍为 0。
#    现判据：cut 降为诊断，不参与 PASS/FAIL。
#
#    ⇒ **窗口本身也是判据前提**，且比参数更隐蔽：同一 conns=16，78s 出 4 次 cut、25s 出 0 次。
#      改窗口 = 改判据可达性，改的人必须回头看这一段。
#      本场景取 78s（ARM_S）正是为了让 B 臂的 cut 有机会出现；调小它会让 ③ 重新变成"不可达"。
#
# ④ **⚠ ③ 的机制在 `39765be` 之后已失效** —— adaptive 不再用 Semaphore
#    死因：③ 推出来的 `avail = max(0, 净add − conns)`、`tick ≥ conns+3` 全部建立在
#          「减半只能归还**空闲** permit」之上（tokio `forget_permits` 只能减空闲、
#          `release` 优先把许可交给排队者 ⇒ 稳态 avail 恒 0）。
#          `39765be` 把 adaptive 换成 **budget 闸门**（`src/download.rs` 的 `gate`），
#          **adaptive 路径上已经没有任何 permit**。
#    现在的 cut 前置只有 `budget > 1` + `delta < last_delta` + 防抖（减半后 2 tick 冷却），
#    **不需要 add 累积** ⇒ 小片（4MB，素材 ≤256MB）时 25s/12 tick 下 conns=16 也能出 cut。
#    另：慢速链路（单片耗时 > 3 个 tick）会在首个 delta 上打一行 `[adaptive]` 告警并
#        **退回固定并发、此后不再 add/cut** ⇒ 那种情况下 **cut 应当为 0**，且这是正确行为。
#    ⇒ **重跑本场景前先确认被测二进制是 39765be 之前还是之后**，两者的预期相反。
#
# ⑤ **慢速守卫的副作用：它按「单片耗时」判，片越大越容易被判粗粒度**（39765be 起）
#    公式：`piece_secs = piece_size × conns ÷ 总吞吐`；而 `piece_size` 由**素材大小**决定
#    （`piece.rs`：>256MB 用 16MB/片，8~256MB 用 4MB/片，<8MB 不分片）。
#    实据（1GB 素材 = 16MB 片，16 连；implementer 实测，我用公式回代一致）：
#        80mbit ⇒ 16×16MB ÷ 10MB/s = **25.6s/片** ⇒ 超过 6s 阈值 ⇒ **起步即退回，add=0 cut=0**
#        8mbit  ⇒ 256s/片                        ⇒ 同样退回
#        要通得过守卫需总吞吐 ≥ 16×16MB ÷ 6s ≈ **42.7 MB/s ≈ 340mbit**
#    ⇒ **AIMD 的实际可用范围比"带宽够快"更窄：还要求「单片 ≤ 3 个 tick」。**
#    1GB 素材在常见家庭带宽（80mbit）下**永远走不到自适应** —— 这是当前设计的适用边界，
#    不是 bug。要覆盖这种组合，只能调阈值、或让片大小随带宽变化（后者属 `piece.rs`，本轮未动）。
#    另：守卫是**一次性判定、只看起步速率**（主理人要求"不刷屏"），
#    所以 A 臂"起步 80mbit 通过、后段降到 8mbit"时**不会再判一次** —— 降速段的粒度已不足，
#    但 tuner 仍会照常 add/cut。这是有意的取舍，不是漏判。
#
# ── 一个必须写死的环境结论（两级，别只记一级）────────────────────────────
#   16 连 + 8mbit 会周期性停摆（B 臂 4/23 tick 为零）并出现回补脉冲（C 臂单 tick 194 Mbps）。
#   一级：**这不是 mc-downloader 的缺陷** —— 低带宽高并发下 netem 队列（limit 1000 包 ≈1.5s）
#          + TCP 拥塞崩溃互相放大，真实窄带链路也是这个形状；换 80mbit 段就不出现。
#   二级：**但该组合永远不能用作吞吐类判据的测量点** —— 要覆盖高并发吞吐，档位必须 ≥ 80mbit。
#
# ⚠ 判据用的吞吐是 CSV 里 `tput_bps`（由 Δbytes/Δt 算），**不是日志里自报的 MB/s**
#   —— 后者是 progress.rs:59 的 `done/secs` 累计平均，阶跃后衰减极慢，会把"已降速"判成"没降"。
#
# ⚠ 每臂用独立目录（$OUT/aimd_<tag>）：三臂互不污染，也不需要清 .part 才能跑下一臂。
#   78s 后由 `timeout` 收工（1GB 在 75s 内下不完是**预期**：525MB 理论量 / 1000MB 素材）。
set -uo pipefail

REAL_BIN="${BIN:-$HOME/mc-downloader/target/release/mc-downloader}"
export BIN="$REAL_BIN"

source "$(dirname "$0")/../lib.sh"
load_env

EXPECT_SHA=e725338708d1ba48048f65b3fb38798461bdbf27   # expected_sha1.sh 1024（1GB 素材，仅用于校验假源自洽）
SIZE_MB=1024
NAME=aimd_step
ARM_S=78            # 每臂观测窗口（25+25+25 + 3s 余量）
SEG_MS=25000

start_origin "$SIZE_MB" "$EXPECT_SHA" || { echo "FAIL $NAME origin_failed"; exit 1; }

# ── 跑一臂：起下载 → 按绝对时刻换挡 → 收工 ─────────────────────────────────
run_arm() {
    local tag="$1" conns="$2" adaptive="$3"
    local dir="$OUT/aimd_$tag" csv="$OUT/logs/aimd_$tag.csv"
    rm -rf "$dir"
    mkdir -p "$dir"

    apply rate 80mbit || return 1          # 段 1
    local t0 t_ms
    t0="$(date +%s%N)"

    bash "$__NF_DIR/trace_rows.sh" "$csv" -- \
        timeout "$ARM_S" "$REAL_BIN" --adaptive="$adaptive" --piece-conns "$conns" \
        fetch "http://127.0.0.1:$PORT/blob.bin" --dir "$dir" --name t.bin &
    local dp=$!

    # 按「距 t0 的绝对毫秒」换挡，避免 sleep 累积漂移
    while :; do
        t_ms=$(( ($(date +%s%N) - t0) / 1000000 ))
        (( t_ms >= SEG_MS )) && break
        sleep 0.2
    done
    apply rate 8mbit || true               # 段 2
    while :; do
        t_ms=$(( ($(date +%s%N) - t0) / 1000000 ))
        (( t_ms >= SEG_MS * 2 )) && break
        sleep 0.2
    done
    apply rate 80mbit || true              # 段 3

    wait "$dp" 2>/dev/null
    return 0
}

# ── 从 CSV 取分段统计：一行 key=value ──────────────────────────────────────
#   段边界：t<23000 → 段1；27000..50000 → 段2；t>=53000 → 段3
#   （每段都掐掉换挡后的 2s 过渡，避免把 netem 切换瞬间算进稳态）
arm_stats() {
    awk -F, '
        NR == 1 { next }
        $2 == "tick" && $3 != "" {
            t = $1 + 0; v = $3 + 0
            if (t >= 27000 && t < 50000)      { s2 += v; n2++; if (v > mx) mx = v; if (v == 0) z2++ }
            else if (t >= 53000)              { s3 += v; n3++; if (v > mx) mx = v }
            else if (t < 23000)               { s1 += v; n1++; if (v > mx) mx = v }
        }
        $2 == "tune" {
            nt++
            if ($4 == "add") na++
            if ($4 == "cut") nc++
            if ($4 == "hold") nh++
            if ($4 == "disabled") ndis++
        }
        END {
            printf "seg1=%.0f seg2=%.0f seg3=%.0f max=%.0f tune=%d add=%d cut=%d hold=%d disabled=%d zero2=%d n2=%d\n",
                   (n1 ? s1/n1 : 0), (n2 ? s2/n2 : 0), (n3 ? s3/n3 : 0), mx, nt, na, nc, nh, ndis, z2, n2
        }' "$1"
}

# 取 stats 行里某个 key 的值。用 grep -o 而不是 sed 的 \b：实测本机 sed 对 `n2=`
# 这种「短键名紧跟数字」的 \b 边界判断会取空（`zero2=4 n2=23` 里 n2 取不到）。
get() { grep -o "$1=[0-9]*" <<<"$2" | head -1 | cut -d= -f2; }

# ── 判据 ───────────────────────────────────────────────────────────────────
check_aimd() {
    local ok=0 detail="" a b c
    local target=8000000 lo hi
    lo=$(( target * 80 / 100 ))
    hi=$(( target * 120 / 100 ))

    a="$(arm_stats "$OUT/logs/aimd_a.csv")"
    b="$(arm_stats "$OUT/logs/aimd_b.csv")"
    c="$(arm_stats "$OUT/logs/aimd_c.csv")"

    local a_seg2 a_seg3 a_add a_cut b_seg2 b_seg3 b_add b_cut b_zero2 b_n2 b_disabled c_seg2 c_max c_tune
    a_seg2="$(get seg2 "$a")"; a_seg3="$(get seg3 "$a")"
    a_add="$(get add "$a")";   a_cut="$(get cut "$a")"
    b_seg2="$(get seg2 "$b")"; b_seg3="$(get seg3 "$b")"
    b_add="$(get add "$b")";   b_cut="$(get cut "$b")"
    b_zero2="$(get zero2 "$b")"; b_n2="$(get n2 "$b")"
    b_disabled="$(get disabled "$b")"
    c_seg2="$(get seg2 "$c")"; c_max="$(get max "$c")"; c_tune="$(get tune "$c")"

    # A：机制双向有效
    if (( a_cut < 1 )); then ok=1; detail+="A:cut=0(expect>=1) "; else detail+="A:cut=$a_cut "; fi
    if (( a_add < 1 )); then ok=1; detail+="A:add=0(expect>=1) "; else detail+="A:add=$a_add "; fi
    # A：降速段收敛
    if (( a_seg2 < lo || a_seg2 > hi )); then
        ok=1; detail+="A:seg2=${a_seg2}(expect ${lo}~${hi}) "
    else detail+="A:seg2=$a_seg2 "; fi
    # A：升速段上升
    # ⚠ A 臂 seg2 测的是「**还能调**」，不是「**调得准**」：
    #   守卫是"一次性判定，只看起步速率" —— 起步 80mbit 时单片 1.6s < 6s 通过，
    #   降到 8mbit 段后单片要 16s（远超 6s），但守卫**不复查**，AIMD 照常 add/cut。
    #   ⇒ 那 95% 的收敛是在"调节粒度其实已不足"的前提下得到的，别把它读成"收敛精度好"。
    if (( a_seg3 <= a_seg2 * 2 )); then
        ok=1; detail+="A:seg3=${a_seg3}(expect>2x seg2) "
    else detail+="A:seg3=$a_seg3 "; fi

    # B：高并发 + 慢链路 ⇒ 应**退回固定并发并告警**（39765be 起的守卫行为）。
    #
    # 判据变更记录（必须留死因）：
    #   原判据「add ≥ 1」在 1GB 素材（16MB 片）+ 16 连 + 80mbit 下**不可能满足** ——
    #   单片 25.6s > 守卫阈值 6s ⇒ 首个 delta 就 act=disabled 并 return，此后 add/cut 恒为 0。
    #   ⇒ 在那个组合下 AIMD **数学上就没有对象可调**，"该不该 cut"是一条没有对象的判据。
    #   ⇒ 改成测**守卫本身是否按设计生效**。
    #
    # ⚠ 本场景**没有 sha 断言**：三臂都在 78s 被 timeout 收工，1GB 素材下不完 ⇒
    #    产物从未 rename 成 t.bin，只有 .part（`.part.state` 40B）。
    #    对未完成文件算 sha 无意义 —— 硬加会得到假 FAIL。
    #    ⭐ 更硬的理由（堵住"那算 .part 的 sha 不就行了"）：`.part` 是**稀疏预分配**的
    #       （`f.set_len(size)`；截至 39765be 在 download.rs:325），未写区域是空洞、**读出来全是 0**：
    #       实测 逻辑 1GB / 实占 416MB（臂A）· 尾 1MiB 非零字节 **0/1048576**。
    #       ⇒ 对 .part 算 sha 只反映"下了多少"，与内容正确性无关。
    #       且**分片路径**的 `rename` 只有全部 worker 正常收工时才执行
    #       （截至 39765be 在 download.rs:590）；单流路径另有一处 `rename`
    #       （流结束后，截至 39765be 在 download.rs:269）。本场景是 1GB/分片路径。
    #       被 timeout 杀掉时进程直接终止，任何一处 rename 都不会发生
    #       ⇒ **不是"不方便"，是"没有对象"**。
    #    内容正确性由批次 3/4 的 assert_sha 覆盖；本档只判「段趋势 + tuner/守卫动作」。
    #    （若将来要在这一档验内容，需加一条小素材的完成腿，例如 8MB / 约 8s。）
    #
    # ⚠ 反向断言（防"因为退回所以没 cut"被读成"因为坏了所以没 cut"）：
    #   cut=0 时判定行必须同时出现 disabled=1 —— 否则两种情形在报告里长得一样。
    local b_raw="$OUT/logs/aimd_b.csv.raw"
    local b_warn=0 b_coarse=0
    if [[ -f "$b_raw" ]]; then
        b_warn="$(grep -c '^\[adaptive\]' "$b_raw" 2>/dev/null)" || b_warn=0
        grep -q 'reason=coarse_grain' "$b_raw" && b_coarse=1
    fi
    [[ "$b_warn" =~ ^[0-9]+$ ]] || b_warn=0

    if (( b_warn != 1 )); then
        ok=1; detail+="B:adaptive_warn=${b_warn}(expect 1) "
    else detail+="B:adaptive_warn=1 "; fi
    if (( b_coarse != 1 )); then
        ok=1; detail+="B:reason=?(expect coarse_grain) "
    else detail+="B:reason=coarse_grain "; fi
    # 退回后：必须有 disabled 行，且 add/cut 都为 0
    if (( b_disabled < 1 )); then
        ok=1; detail+="B:disabled=0(expect>=1) "
    else detail+="B:disabled=$b_disabled "; fi
    if (( b_add != 0 || b_cut != 0 )); then
        ok=1; detail+="B:add=$b_add,cut=$b_cut(expect 0/0：已退回固定并发) "
    else detail+="B:add=0 B:cut=0 "; fi
    detail+="B:seg2=$b_seg2 B:zero2=$b_zero2/$b_n2 "
    if (( b_seg2 <= 0 )); then
        ok=1; detail+="B:seg2=0(无数据) "; fi
    if (( b_seg3 <= b_seg2 * 2 )); then
        ok=1; detail+="B:seg3=${b_seg3}(expect>2x seg2) "
    else detail+="B:seg3=$b_seg3 "; fi

    # C：对照（关 adaptive）—— 硬判据只有「tune 行 = 0」（证明钩子确实没在跑）。
    # 不判「max ≤ piece_conns × 8mbit」：实测该 max 出现在 t=64.4s，而它前面有连续 5 个
    # tick 是 **0 吞吐**（8mbit 段把 16 条流压死后 TCP 停摆，恢复时把积压一次性送出）——
    # 那是**回补脉冲**，不是持续速率。拿 max 判"不超上限"测的是脉冲，与命题无关。
    # ⇒ 记录 seg2 均值（8mbit 窗口）+ max 作诊断，两者都不参与判定。
    if (( c_tune != 0 )); then
        ok=1; detail+="C:tune=${c_tune}(expect 0) "
    else detail+="C:tune=0 "; fi
    detail+="C:seg2=$c_seg2 C:max=$c_max(诊断) "
    if (( c_seg2 <= 0 )); then
        ok=1; detail+="C:seg2=0(8mbit 段无数据) "; fi

    ASSERT_DETAIL="$detail"
    return $ok
}

# ── 三臂 ───────────────────────────────────────────────────────────────────
echo "[*] 臂 A：--piece-conns 1（AIMD 双向有效）" >&2
run_arm a 1  true  || { echo "FAIL $NAME armA_setup_failed"; exit 1; }
echo "[*] 臂 B：--piece-conns 16（高并发回归）" >&2
run_arm b 16 true  || { echo "FAIL $NAME armB_setup_failed"; exit 1; }
echo "[*] 臂 C：--piece-conns 16 + --adaptive=false（对照）" >&2
run_arm c 16 false || { echo "FAIL $NAME armC_setup_failed"; exit 1; }

rc=0
check_aimd || rc=1
if (( rc == 0 )); then
    __nf_verdict PASS "$NAME" "$ASSERT_DETAIL"
else
    __nf_verdict FAIL "$NAME" "$ASSERT_DETAIL"
fi

# CSV 留档（报告要引用），路径写进 stderr 便于人工复核
echo "[*] traces: $OUT/logs/aimd_a.csv $OUT/logs/aimd_b.csv $OUT/logs/aimd_c.csv" >&2
cp -f "$OUT/logs/aimd_a.csv" /tmp/aimd_trace.csv 2>/dev/null || true

clear
exit $rc
