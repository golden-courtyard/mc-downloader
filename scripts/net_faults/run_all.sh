#!/usr/bin/env bash
# ============================================================================
# scripts/net_faults/run_all.sh —— 遍历 scenes、跑 N 轮、比对结论、出报告
#
# 用法：
#   bash run_all.sh                     # 跑全部 14 个场景，各 1 轮
#   bash run_all.sh --repeat 3          # spec §4.2 要求：3 轮结论必须一致
#   bash run_all.sh --only l2_rate      # 按名字子串过滤（调试用，不改判据）
#
# 为什么放在 net_faults/ 而不是 scenarios/：本脚本会 glob `scenarios/*.sh`，
#   放进被 glob 的目录里会把自己当成场景跑一遍。
#
# 八件事（对应 tasks T6.1）：
#   ① 恰好 14 个场景，否则报错退出（防"少跑一个也全绿"）
#   ② `# ASSERT:` 元数据缺失 → 不执行，直接 FAIL missing_assert（spec §5.5）
#   ③ --repeat N：连续跑 N 轮，逐轮比对**结论**；不一致 → FAIL unstable
#   ④ --only <子串>：过滤，不改判据
#   ⑤ report.md：一场景一行
#   ⑥ 收尾残留自检三处（tc / iptables / mtu），任一不干净 → 整体 FAIL(residue)
#   ⑦ 耗时断言：实测 ≤ TIMEOUT_S 且 TIMEOUT_S ≤ 300
#   ⑧ 汇总行
#
# 判定行的来源：场景自己打印 `PASS|FAIL <name> ...`（多数走 lib 的 run_case，
#   AIMD 自己编排三臂后直接出**一行**）⇒ 本脚本取**最后一条**匹配 `^(PASS|FAIL) ` 的行。
#
# ⚠ 比对的是**结论**（PASS/FAIL），不是整行 —— 整行含实测数值，天然每轮不同。
# ⚠ 每轮记起止时间：24 分钟的 --repeat 3 会跨越机器负载波动，出现 unstable 时
#   先看时间戳判断是不是环境漂移，别直接判产品不稳。
# ============================================================================
set -uo pipefail

HERE="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SCEN_DIR="$HERE/scenarios"
LOG_DIR="$HERE/logs"
REPORT="$HERE/report.md"
EXPECTED_SCENARIOS=14
MAX_TIMEOUT_S=300

REPEAT=1
ONLY=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --repeat) REPEAT="${2:-}"; shift 2 ;;
        --only)   ONLY="${2:-}";   shift 2 ;;
        -h|--help) sed -n '2,20p' "${BASH_SOURCE[0]}" | sed -e 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "run_all: 未知参数 $1" >&2; exit 2 ;;
    esac
done
[[ "$REPEAT" =~ ^[1-9][0-9]*$ ]] || { echo "run_all: --repeat 需要正整数（收到 '${REPEAT}'）" >&2; exit 2; }

# ── ① 场景发现与计数 ───────────────────────────────────────────────────────
mapfile -t ALL < <(find "$SCEN_DIR" -maxdepth 1 -type f -name '*.sh' -printf '%f\n' | sort)
if (( ${#ALL[@]} != EXPECTED_SCENARIOS )); then
    echo "run_all: 场景数不对 —— 期望 ${EXPECTED_SCENARIOS}，实际 ${#ALL[@]}" >&2
    printf '  %s\n' "${ALL[@]}" >&2
    exit 1
fi

SELECTED=()
for f in "${ALL[@]}"; do
    [[ -n "$ONLY" && "$f" != *"$ONLY"* ]] && continue
    SELECTED+=("$f")
done
if (( ${#SELECTED[@]} == 0 )); then
    echo "run_all: --only '$ONLY' 没匹配到任何场景" >&2
    exit 1
fi

mkdir -p "$LOG_DIR"

meta() {   # $1=file $2=key  → 头部 `# KEY: value` 的值
    sed -n "s/^# *${2}: *//p" "$1" | head -1
}
now_hms() { date +%H:%M:%S; }
ms_of() { local t="${EPOCHREALTIME//[!0-9]/}"; printf '%s' $(( t / 1000 )); }

T_START="$(ms_of)"
T0_HMS="$(now_hms)"

# ── 结果表（并行数组，按 SELECTED 顺序）────────────────────────────────────
# ⚠ 必须写 =()：bash 5.3 下 `declare -a X` 之后 `${#X[@]}` 在 set -u 里仍报 unbound
declare -a R_NAME=() R_VERDICT=() R_KEY=() R_ELAPSED=() R_CONSIST=() R_TIMES=() R_NOTE=()
declare -i N_PASS=0 N_FAIL=0

for f in "${SELECTED[@]}"; do
    name="${f%.sh}"
    path="$SCEN_DIR/$f"
    assert_meta="$(meta "$path" ASSERT)"
    timeout_meta="$(meta "$path" TIMEOUT_S)"

    # 按索引写入（不用 +=）：七个数组必须**同长**，否则报告会整体错位
    idx=${#R_NAME[@]}
    R_NAME[idx]="$name"
    R_VERDICT[idx]="FAIL"
    R_KEY[idx]=""
    R_ELAPSED[idx]="-"
    R_CONSIST[idx]="-"
    R_TIMES[idx]="-"
    R_NOTE[idx]=""

    # ② ASSERT 缺失：不执行
    if [[ -z "$assert_meta" ]]; then
        R_KEY[idx]="missing_assert"; R_NOTE[idx]="头部缺 # ASSERT:"
        N_FAIL+=1
        echo "FAIL $name missing_assert" >&2
        continue
    fi
    # ⑦ 元数据合法性
    if [[ ! "$timeout_meta" =~ ^[0-9]+$ ]]; then
        R_KEY[idx]="bad_timeout_meta"; R_NOTE[idx]="# TIMEOUT_S 缺失或非数字"
        N_FAIL+=1
        echo "FAIL $name bad_timeout_meta" >&2
        continue
    fi
    if (( timeout_meta > MAX_TIMEOUT_S )); then
        R_KEY[idx]="timeout_meta=${timeout_meta}s"
        R_NOTE[idx]="TIMEOUT_S > ${MAX_TIMEOUT_S}（spec §4.1）"
        N_FAIL+=1
        echo "FAIL $name timeout_meta" >&2
        continue
    fi

    # ③ N 轮
    verdicts=(); elapseds=(); stamps=()
    max_el=0
    last_v=""
    for ((r = 1; r <= REPEAT; r++)); do
        log="$LOG_DIR/$name.r$r.log"
        st_hms="$(now_hms)"
        t0="$(ms_of)"
        # 硬兜底：比 TIMEOUT_S 多给 60s，防某个场景卡死拖垮整轮
        timeout $((timeout_meta + 60)) bash "$path" >"$log" 2>&1
        rc=$?
        el=$(( $(ms_of) - t0 ))
        (( el > max_el )) && max_el=$el
        elapseds+=("$el")
        stamps+=("${st_hms}→$(now_hms)")

        # 持久化本轮快照：$OUT 默认 /tmp/net_faults，**重启会清空**（2026-10-22 已丢过一次）。
        # 只在场景产生了 snap-* 时搬（目前仅多轮场景如 aimd_step 会生成）。
        for d in "${OUT:-/tmp/net_faults}"/logs/snap-*; do
            [[ -d "$d" ]] || continue
            mkdir -p "$HOME/net_faults/artifacts" 2>/dev/null || true
            cp -r "$d" "$HOME/net_faults/artifacts/" 2>/dev/null || true
        done

        v="$(grep -E '^(PASS|FAIL) ' "$log" | tail -1)"
        if [[ -z "$v" ]]; then
            verdicts+=("FAIL")
            R_NOTE[idx]="${R_NOTE[idx]}第${r}轮无判定行 rc=${rc}（$log）; "
        else
            verdicts+=("$(cut -d' ' -f1 <<<"$v")")
            last_v="$v"
        fi
    done

    # 结论一致性：比对 PASS/FAIL，不比数值（数值每轮天然不同）
    if (( $(printf '%s\n' "${verdicts[@]}" | sort -u | wc -l) == 1 )); then
        R_CONSIST[idx]="ok(${REPEAT}轮一致)"
        final="${verdicts[${#verdicts[@]}-1]}"
    else
        R_CONSIST[idx]="unstable($(printf '%s' "${verdicts[*]}" | tr ' ' ','))"
        final="FAIL"
        R_NOTE[idx]="${R_NOTE[idx]}结论不一致: ${verdicts[*]}; "
    fi

    # ⑦ 耗时断言（毫秒比较）
    if (( max_el > timeout_meta * 1000 )); then
        final="FAIL"
        R_NOTE[idx]="${R_NOTE[idx]}overtime: ${max_el}ms > TIMEOUT_S ${timeout_meta}s; "
    fi

    R_VERDICT[idx]="$final"
    if [[ -n "$last_v" ]]; then
        R_KEY[idx]="$(sed -e "s/^\(PASS\|FAIL\) ${name} *//" <<<"$last_v" | sed -e 's/|/\\|/g')"
    fi
    R_ELAPSED[idx]="$(IFS=/; printf '%sms' "${elapseds[*]}")"
    R_TIMES[idx]="$(printf '%s' "${stamps[*]}" | sed -e 's/ /; /g')"

    if [[ "$final" == "PASS" ]]; then
        N_PASS+=1
    else
        N_FAIL+=1
        echo "FAIL $name" >&2
    fi
done

# ── ⑥ 收尾残留自检（三处）──────────────────────────────────────────────────
res_ok=1
res_txt=""
tc_out="$(tc qdisc show dev lo 2>/dev/null)"
if grep -q netem <<<"$tc_out"; then
    res_ok=0; res_txt+="tc: ${tc_out//$'\n'/;} "
fi
ipt_n="$(sudo -n /usr/sbin/iptables -L OUTPUT -n 2>/dev/null | wc -l)"
if (( ipt_n > 2 )); then
    res_ok=0; res_txt+="iptables: ${ipt_n} 行 "
fi
mtu="$(ip -o link show dev lo 2>/dev/null | grep -o 'mtu [0-9]*' | awk '{print $2}')"
if [[ "$mtu" != "65536" ]]; then
    res_ok=0; res_txt+="mtu: ${mtu:-空} "
fi

# ── ⑤ 报告 ─────────────────────────────────────────────────────────────────
{
    echo "| 场景 | 结论 | 关键指标 | 耗时 | 轮次一致性 | 轮次起止 | 死因/备注 |"
    echo "|---|---|---|---|---|---|---|"
    for i in "${!R_NAME[@]}"; do
        printf '| %s | %s | %s | %s | %s | %s | %s |\n' \
            "${R_NAME[$i]}" "${R_VERDICT[$i]}" "${R_KEY[$i]:-}" "${R_ELAPSED[$i]}" \
            "${R_CONSIST[$i]}" "${R_TIMES[$i]}" "${R_NOTE[$i]:-}"
    done
} >"$REPORT"

if (( res_ok == 0 )); then
    {
        echo
        echo "**残留自检未通过**（整体 FAIL(residue)）：${res_txt}"
        echo '```'
        echo "tc qdisc show dev lo:"; tc qdisc show dev lo 2>&1
        echo "iptables -L OUTPUT -n:"; sudo -n /usr/sbin/iptables -L OUTPUT -n 2>&1
        echo "ip link show lo:"; ip link show lo 2>&1 | head -1
        echo '```'
    } >>"$REPORT"
fi

# ── ⑧ 汇总行 ───────────────────────────────────────────────────────────────
T_END="$(ms_of)"
wall=$(( (T_END - T_START) / 1000 ))
printf 'TOTAL %d/%d passed · repeat=%d · 挂钟 %ss\n' "$N_PASS" "${#SELECTED[@]}" "$REPEAT" "$wall"
[[ -n "$ONLY" ]] && printf '（--only %s，场景总数仍校验为 %d）\n' "$ONLY" "$EXPECTED_SCENARIOS"
if (( res_ok == 0 )); then
    printf 'RESIDUE: %s\n' "$res_txt"
fi
if (( N_FAIL > 0 )); then
    printf 'FAILED:'
    for i in "${!R_NAME[@]}"; do
        [[ "${R_VERDICT[$i]}" == "FAIL" ]] && printf ' %s' "${R_NAME[$i]}"
    done
    printf '\n'
fi
printf 'report: %s（%d 行；表头 1 + 分隔 1 + %d 数据行）\n' \
    "$REPORT" "$(wc -l <"$REPORT")" "${#R_NAME[@]}"

(( N_FAIL == 0 && res_ok == 1 )) && exit 0
exit 1
