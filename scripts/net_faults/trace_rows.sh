#!/usr/bin/env bash
# ============================================================================
# scripts/net_faults/trace_rows.sh —— 把「进度行」与「[tune] 行」对齐成一张 CSV
#
# 这只是**生成骨架**（T5.1 的 aimd_trace.csv 由它产出），不含任何 AIMD 判据。
#
# 用法：
#   trace_rows.sh <输出csv> -- <被测命令...>
#
# ── 三个必须这么做的理由（都是实测/读码得来的，不是设计偏好）─────────────
#
# 1) 两类行**都没有时间戳**：进度行在 stdout、`[tune]` 行在 stderr，各自只有内容。
#    ⇒ 必须在**读到的瞬间**打时间戳，事后无法补。故合并 `2>&1` 成一条时间轴。
#
# 2) `tput_bps` **不能用程序自报的 `MB/s`**：`progress.rs:59` 是
#    `speed = done / secs`（secs = 自启动以来的秒数）⇒ 那是**累计平均**。
#    阶跃降速后它只会缓慢衰减，"收敛到 8mbit"会被判成"没降下来"。
#    ⇒ 本表用**相邻两个 tick 的 Δbytes/Δt** 算瞬时值（首行无前值，留空）。
#
# 3) 进度行有**四种单位**（`progress.rs:77` / `:91`）：
#      done/total：`B`(无小数) / `KB`(1) / `MB`(1) / `GB`(2)
#      rate      ：`B`(无小数) / `KB`(1) / `MB`(2)   ← 本表不用它
#    8mbit 档实测会打出 `[ 918.9 KB / 8.0 MB ]  916.4 KB/s` —— 只认 MB 的解析器会
#    把低速段的 tick 全部丢掉（我第一版就丢光了）。
#
# 4) 收尾行没有速率：`[ 8.0 MB / 8.0 MB ]  完成`（`progress.rs:71`）。
#    本表仍产出一行 tick（bytes 为最终值），tput 由 Δ 算 —— 若 Δt 过小会偏大，
#    但判据只看分段趋势，不受影响。
#
# 列：t_ms, evt, tput_bps, act, budget, take, inflight, delta, last, bytes
#   evt ∈ {tick, tune}
#   tick：tput_bps / bytes 有值；其余空
#   tune：act/budget/take/inflight/delta/last 有值；tput_bps 空；
#         bytes = 最近一次 tick 的累计字节（便于把 tune 挂回进度）
#
# ⚠ **日志格式随代码版本变，本脚本兼容两版**（按字段名匹配，不按位置切）：
#   旧（39765be 之前）：`[tune] act= avail= delta= last=`
#   新（39765be 起）：  `[tune] act= budget= take= inflight= delta= last=`
#   按位置切会在两版之间静默错列 —— 这正是批次 6 踩过的"报告整体错位一行"同族。
#
# ⚠ **两版的字段集合不同**：旧 5 个 token（含 `[tune]` 前缀）= 4 个键值对
#   （act/avail/delta/last）；新 7 个 token = 6 个键值对（act/budget/take/inflight/delta/last）。
#   ⇒ 解析**不能假设列数固定**，也**不能假设 `avail` 一定在**；
#     而**输出 CSV 的列是固定 10 列**，某版没有的字段一律留空。
#
# ⚠ `avail` 在 `39765be` 之后**不存在了**（不是"暂时为空"）：adaptive 路径已改用
#   budget 闸门，`Semaphore` 只在 `--adaptive=false` 时构造，permit 这个概念就没了。
#   取而代之：
#     `budget`   = 当前允许的并发上限（**这个才代表"当前并发"**，旧版的 avail 无法对应）
#     `take`     = 本次 cut 减掉几个（只在 act=cut 时非零）
#     `inflight` = 当刻在飞的请求数
#
# ⚠ 三者**都只作诊断**，不得进断言：
#   `inflight` 会**瞬时超调**（先 fetch_add 再判断），单次采样可能看到 budget+2；
#   凡是要进断言的量必须稳定可复现 —— 做不到就别让它进，加容差只是把噪声阈值化。
#   （若将来非用不可，下限是 `inflight <= budget + 2`，见 download.rs 的取值顺序）
#
# 副作用：本脚本自己设 MC_TUNE_LOG=1，并把合并后的原始流另存为 `<csv>.raw`
#   （`check_aimd` 要用它数 `[adaptive]` 告警行），最后原样透传退出码。
# ============================================================================
set -uo pipefail

if [[ $# -lt 3 || "$2" != "--" ]]; then
    echo "用法：trace_rows.sh <输出csv> -- <被测命令...>" >&2
    exit 2
fi

OUT_CSV="$1"
shift 2
cmd=("$@")

# 微秒级时间戳，不 fork。EPOCHREALTIME 的小数点受 locale 影响 → 先剥掉所有非数字
__now_ms() {
    local us="${EPOCHREALTIME//[!0-9]/}"
    printf '%s' $(( us / 1000 ))
}

__to_bytes() {   # $1=数值 $2=单位
    awk -v v="$1" -v u="$2" 'BEGIN{
        m = (u=="B") ? 1 : (u=="KB") ? 1024 : (u=="MB") ? 1048576 : 1073741824;
        printf "%d", v * m
    }'
}

T0="$(__now_ms)"
printf 't_ms,evt,tput_bps,act,budget,take,inflight,delta,last,bytes\n' >"$OUT_CSV"

prev_t="" prev_bytes="" last_bytes=""

MC_TUNE_LOG=1 "${cmd[@]}" 2>&1 | tee "$OUT_CSV.raw" | while IFS= read -r line; do
    t=$(( $(__now_ms) - T0 ))

    case "$line" in
        "[tune]"*)
            act="" budget="" take="" inflight="" delta="" last=""
            for kv in $line; do
                case "$kv" in
                    act=*)      act="${kv#act=}" ;;
                    budget=*)   budget="${kv#budget=}" ;;
                    take=*)     take="${kv#take=}" ;;
                    inflight=*) inflight="${kv#inflight=}" ;;
                    delta=*)    delta="${kv#delta=}" ;;
                    last=*)     last="${kv#last=}" ;;
                esac
            done
            printf '%s,tune,,%s,%s,%s,%s,%s,%s,%s\n' \
                "$t" "$act" "$budget" "$take" "$inflight" "$delta" "$last" "$last_bytes"
            ;;
        \[[[:space:]]*[0-9]*)
            # [ 918.9 KB / 8.0 MB ]  916.4 KB/s  ETA 00:07
            # [ 8.0 MB / 8.0 MB ]  完成
            if [[ "$line" =~ ^\[[[:space:]]+([0-9]+(\.[0-9]+)?)[[:space:]]+(B|KB|MB|GB)[[:space:]]+/[[:space:]]+([0-9]+(\.[0-9]+)?)[[:space:]]+(B|KB|MB|GB)[[:space:]]+\] ]]; then
                cur="$(__to_bytes "${BASH_REMATCH[1]}" "${BASH_REMATCH[3]}")"
                tput=""
                if [[ -n "$prev_bytes" && -n "$prev_t" ]]; then
                    dt=$(( t - prev_t ))
                    if (( dt > 0 && cur >= prev_bytes )); then
                        tput="$(awk -v a="$cur" -v b="$prev_bytes" -v ms="$dt" \
                            'BEGIN{printf "%d", (a-b)*8*1000/ms}')"
                    fi
                fi
                printf '%s,tick,%s,,,,,,,%s\n' "$t" "$tput" "$cur"
                prev_t="$t"
                prev_bytes="$cur"
                last_bytes="$cur"
            fi
            ;;
        *)
            : # [*] URL / [1/1] t.bin / [+] 完成 -> ... 不进表
            ;;
    esac
done >>"$OUT_CSV"

rc=${PIPESTATUS[0]}
printf '# rows: tick=%s tune=%s → %s\n' \
    "$(grep -c ',tick,' "$OUT_CSV")" "$(grep -c ',tune,' "$OUT_CSV")" "$OUT_CSV" >&2
exit "$rc"
